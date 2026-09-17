//! Qual VM é o programa em depuração.
//!
//! O servidor carrega várias VMs — o gamemode e cada filterscript —, e o plugin
//! recebe todas. O bloco de debug, os breakpoints e a inspeção são de **um**
//! `.amx`, e os endereços de código começam em zero em cada VM: aplicados a outra
//! VM, um breakpoint pararia em código dela, mostrado com as linhas e as
//! variáveis do programa depurado.
//!
//! A VM é reconhecida pelo conteúdo, comparada com o `.amx` que a sessão passou:
//! o cabeçalho e as tabelas de publics e de nomes, que o servidor não altera ao
//! carregar. O código fica de fora (é relocado na carga), e a tabela de natives
//! também (recebe os endereços quando os plugins registram funções).

/// Tamanho do `AMX_HEADER`, em bytes.
pub const HEADER_LEN: usize = 56;

/// O que identifica um programa Pawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint {
    /// Campos do cabeçalho que descrevem a imagem: tamanhos e posições das
    /// seções e das tabelas. `size` e `flags` ficam de fora — `flags` muda na
    /// carga, e `size` é o do arquivo, que pode estar compactado.
    layout: [i32; 11],
    /// A tabela de publics: posição no código e nome de cada uma.
    publics: Vec<u8>,
    /// Os nomes de publics, natives e variáveis públicas.
    names: Vec<u8>,
}

impl Fingerprint {
    /// Lê a identidade de uma imagem que começa no cabeçalho e vai pelo menos
    /// até o início do código — o arquivo `.amx` inteiro, ou a memória de uma
    /// VM carregada. `None` se não for uma imagem AMX coerente.
    #[must_use]
    pub fn read(image: &[u8]) -> Option<Self> {
        let field = |at: usize| -> Option<i32> {
            Some(i32::from_le_bytes(image.get(at..at + 4)?.try_into().ok()?))
        };
        let magic = u16::from_le_bytes(image.get(4..6)?.try_into().ok()?);
        // Magia da AMX de 32 bits (`AMX_MAGIC_32`).
        if magic != 0xF1E0 {
            return None;
        }
        let defsize = i16::from_le_bytes(image.get(10..12)?.try_into().ok()?);
        let [
            cod,
            dat,
            hea,
            stp,
            cip,
            publics,
            natives,
            libraries,
            pubvars,
            tags,
            nametable,
        ] = [12, 16, 20, 24, 28, 32, 36, 40, 44, 48, 52].map(field);
        let (cod, publics, natives, nametable) = (cod?, publics?, natives?, nametable?);
        let offset = |value: i32| usize::try_from(value).ok();
        let publics_table = image.get(offset(publics)?..offset(natives)?)?.to_vec();
        let names = image.get(offset(nametable)?..offset(cod)?)?.to_vec();
        Some(Self {
            layout: [
                i32::from(defsize),
                cod,
                dat?,
                hea?,
                stp?,
                cip?,
                publics,
                natives,
                libraries?,
                pubvars?,
                tags?,
            ],
            publics: publics_table,
            names,
        })
    }

    /// Onde o código começa: até ali vai o que a identidade compara.
    #[must_use]
    pub const fn code_start(image_header: &[u8; HEADER_LEN]) -> i32 {
        i32::from_le_bytes([
            image_header[12],
            image_header[13],
            image_header[14],
            image_header[15],
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Uma imagem AMX mínima: cabeçalho, uma public e a tabela de nomes, com o
    /// código logo depois.
    fn image(public_name: &str, flags: i16) -> Vec<u8> {
        let publics = HEADER_LEN;
        let natives = publics + 8;
        let nametable = natives;
        let mut names = vec![0x1F, 0x00]; // comprimento máximo de nome
        let name_at = nametable + names.len();
        names.extend_from_slice(public_name.as_bytes());
        names.push(0);
        let cod = nametable + names.len();

        let mut b = Vec::new();
        let push = |b: &mut Vec<u8>, v: i32| b.extend_from_slice(&v.to_le_bytes());
        push(&mut b, 999); // size
        b.extend_from_slice(&0xF1E0u16.to_le_bytes());
        b.push(8);
        b.push(8);
        b.extend_from_slice(&flags.to_le_bytes());
        b.extend_from_slice(&8i16.to_le_bytes()); // defsize
        let at = |v: usize| i32::try_from(v).unwrap();
        for v in [
            at(cod),
            at(cod) + 40,
            at(cod) + 80,
            at(cod) + 4000,
            -1,
            at(publics),
            at(natives),
            at(natives),
            at(natives),
            at(natives),
            at(nametable),
        ] {
            push(&mut b, v);
        }
        assert_eq!(b.len(), HEADER_LEN);
        push(&mut b, 12); // endereço da public no código
        push(&mut b, at(name_at));
        b.extend_from_slice(&names);
        b.extend_from_slice(&[0xAA; 40]); // código
        b
    }

    #[test]
    fn the_file_and_the_loaded_image_are_the_same_program() {
        let file = image("OnGameModeInit", 0);
        // Na memória a carga liga flags (relocação) e o código muda.
        let mut loaded = image("OnGameModeInit", 0x0004);
        let code = usize::try_from(Fingerprint::code_start(
            &loaded[..HEADER_LEN].try_into().unwrap(),
        ))
        .unwrap();
        loaded[code..].fill(0x55);
        assert_eq!(Fingerprint::read(&file), Fingerprint::read(&loaded));
    }

    /// Gamemode e filterscript com o mesmo formato de código diferem nas
    /// publics — o caso reproduzido no servidor real. Nomes do mesmo tamanho:
    /// o que distingue é o conteúdo da tabela, não uma posição deslocada.
    #[test]
    fn a_filterscript_is_not_the_gamemode() {
        let gamemode = Fingerprint::read(&image("OnGameModeInit", 0));
        let filterscript = Fingerprint::read(&image("OnGameModeExit", 0));
        assert!(gamemode.is_some());
        assert_ne!(gamemode, filterscript);
    }

    #[test]
    fn what_is_not_an_amx_has_no_fingerprint() {
        assert_eq!(Fingerprint::read(b"sem magia"), None);
        let mut broken = image("OnGameModeInit", 0);
        broken.truncate(HEADER_LEN + 2);
        assert_eq!(Fingerprint::read(&broken), None, "tabelas cortadas");
    }
}

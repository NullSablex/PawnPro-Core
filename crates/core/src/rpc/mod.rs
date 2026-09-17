//! JSON-RPC sobre stdio, uma mensagem por linha.
//!
//! A extensão escreve uma requisição no stdin e lê a resposta no stdout. O
//! mesmo canal leva as notificações do supervisor.

pub mod compiler;
pub mod config;
pub mod debugger;
pub mod diagnostics;
pub mod engine;
pub mod handlers;
pub mod includes;
pub mod protocol;
pub mod server;
pub mod state;

use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex};

use serde_json::json;

use crate::config::{ConfigManager, ConfigService, Snapshot};
use crate::gateway::Gateway;
use crate::supervisor::debugger::DebuggerService;
use crate::supervisor::engine::EngineService;

use protocol::{Notification, Outgoing, Request, RequestId, Response, ResponseError};

/// Escreve mensagens no stdout, uma por linha.
///
/// Sem o `Mutex`, as linhas do laço principal e do supervisor se
/// intercalariam.
#[derive(Clone)]
pub struct Sender {
    out: Arc<Mutex<Box<dyn Write + Send>>>,
}

impl Sender {
    #[must_use]
    pub fn new(out: Box<dyn Write + Send>) -> Self {
        Self {
            out: Arc::new(Mutex::new(out)),
        }
    }

    /// Falha de escrita é ignorada: significa que a extensão fechou o canal,
    /// e o laço principal encerra ao ler EOF.
    pub fn send(&self, message: &Outgoing) {
        let Ok(line) = serde_json::to_string(message) else {
            return;
        };
        if let Ok(mut out) = self.out.lock() {
            let _ = writeln!(out, "{line}");
            let _ = out.flush();
        }
    }

    /// Envia uma notificação.
    pub fn notify(&self, method: &str, params: serde_json::Value) {
        self.send(&Outgoing::Notification(Notification::new(method, params)));
    }
}

/// O estado do processo que os métodos precisam: a configuração do projeto
/// aberto, a engine que ela alimenta, o depurador e o soquete único por onde
/// os dois atendem.
///
/// A ordem dos campos é a do `Drop`: os subsistemas saem antes do gateway, que
/// é quem encerra o runtime onde as conexões deles vivem.
pub struct Services {
    pub config: Arc<ConfigService>,
    pub engine: EngineService,
    pub debugger: DebuggerService,
    pub gateway: Arc<Gateway>,
}

impl Services {
    /// Liga as peças, com a configuração global do diretório do usuário.
    #[must_use]
    pub fn new(sender: &Sender) -> Self {
        Self::with_config(sender, ConfigService::new())
    }

    /// Liga as peças: a engine assina a configuração, e a extensão é avisada
    /// de toda mudança — a que ela pediu e a que veio do disco.
    #[must_use]
    pub fn with_config(sender: &Sender, config: Arc<ConfigService>) -> Self {
        config.subscribe(Box::new(apply_diagnostics));
        let notifier = sender.clone();
        config.subscribe(Box::new(move |manager| {
            if let Ok(snapshot) = serde_json::to_value(Snapshot::of(manager)) {
                notifier.notify("config.changed", snapshot);
            }
        }));
        let gateway = Gateway::new();
        let engine = EngineService::with_config(Arc::clone(&config), Arc::clone(&gateway));
        let debugger = DebuggerService::new(Arc::clone(&gateway));
        Self {
            config,
            engine,
            debugger,
            gateway,
        }
    }
}

/// O nível do registro vem da configuração do projeto e vale sem reiniciar
/// nada.
///
/// Fica fora do `ConfigService` porque o nível é estado do processo inteiro:
/// nos testes do serviço, aplicá-lo disputaria com os testes do registro que
/// rodam ao mesmo tempo.
fn apply_diagnostics(manager: &ConfigManager) {
    use crate::diagnostics::{Level, configure, ignore_logs};
    let level = Level::from_name(&manager.get_all().diagnostics.level);
    configure(manager.project_root(), level);
    if level != Level::Off {
        ignore_logs(manager.project_root());
    }
}

/// Um trabalho demorado, que responde quando terminar.
pub type Job = Box<dyn FnOnce() -> Result<serde_json::Value, ResponseError> + Send>;

/// O trabalho de um método que não pode ocupar o laço, se for um deles.
///
/// O laço atende um pedido por vez: um método que leva segundos, rodado ali,
/// faria todos os outros esperarem.
fn background_job(
    method: &str,
    params: &serde_json::Value,
    services: &Services,
) -> Option<Result<Job, ResponseError>> {
    match method {
        "compiler.run" => Some(compiler::run_job(params, &services.config)),
        _ => None,
    }
}

/// Roda o laço de mensagens até o stdin fechar.
///
/// Uma linha malformada não derruba o processo.
///
/// # Errors
/// Falha de leitura do stdin.
pub fn serve<R: BufRead>(input: R, sender: &Sender, services: &Services) -> std::io::Result<()> {
    for line in input.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let Ok(request) = serde_json::from_str::<Request>(trimmed) else {
            // Sem `id` recuperável não há a quem responder.
            crate::diag_warn!("core/rpc", "linha ignorada, não é uma requisição válida");
            continue;
        };

        if let Some(job) = background_job(&request.method, &request.params, services) {
            let (reply_to, method, id) =
                (sender.clone(), request.method.clone(), request.id.clone());
            let spawned = std::thread::Builder::new()
                .name(format!("pawnpro-rpc-{method}"))
                .spawn(move || {
                    let result = job.and_then(|job| job());
                    if let Some(id) = id {
                        reply_to.send(&Outgoing::Response(respond(&method, id, result)));
                    } else if let Err(error) = result {
                        crate::diag_warn!(
                            "core/rpc",
                            "notificação `{method}` falhou: {}",
                            error.message
                        );
                    }
                });
            if let Err(error) = spawned {
                crate::diag_error!("core/rpc", "{} sem thread: {error}", request.method);
                if let Some(id) = request.id.clone() {
                    let error =
                        ResponseError::internal(&format!("sem thread para `{}`", request.method));
                    sender.send(&Outgoing::Response(Response::err(id, error)));
                }
            }
            continue;
        }

        let Some(id) = request.id.clone() else {
            // Notificação: executa e não responde. Sem resposta pelo protocolo,
            // uma falha só fica registrada no log.
            if let Err(error) = handle(&request.method, &request.params, sender, services) {
                crate::diag_warn!(
                    "core/rpc",
                    "notificação `{}` falhou: {}",
                    request.method,
                    error.message
                );
            }
            continue;
        };

        let result = handle(&request.method, &request.params, sender, services);
        sender.send(&Outgoing::Response(respond(&request.method, id, result)));
    }
    Ok(())
}

/// A resposta a um pedido, com o registro de como terminou.
fn respond(
    method: &str,
    id: RequestId,
    result: Result<serde_json::Value, ResponseError>,
) -> Response {
    match result {
        Ok(value) => {
            // A sondagem da porta se repete a cada poucos segundos enquanto o
            // painel está aberto: registrá-la afogaria o resto.
            if !is_polling(method) {
                crate::diag_info!("core/rpc", "{method} atendido");
            }
            Response::ok(id, value)
        }
        Err(error) => {
            crate::diag_error!(
                "core/rpc",
                "{method} recusado ({}): {}",
                error.code,
                error.message
            );
            Response::err(id, error)
        }
    }
}

/// Executa um método, incluindo os do próprio core.
fn handle(
    method: &str,
    params: &serde_json::Value,
    sender: &Sender,
    services: &Services,
) -> Result<serde_json::Value, ResponseError> {
    match method {
        // A extensão pergunta o que esta versão suporta, em vez de assumir.
        "core.version" => Ok(json!({
            "version": env!("CARGO_PKG_VERSION"),
            "methods": method_names(),
        })),
        other if other.starts_with("compiler.") => {
            compiler::dispatch(other, params, &services.config)
        }
        other if other.starts_with("config.") => config::dispatch(other, params, &services.config),
        other if other.starts_with("engine.") => {
            engine::dispatch(other, params, sender, &services.engine)
        }
        "debug.start" => debugger::dispatch(method, sender, &services.debugger),
        "server.resolve" => server::dispatch(method, params, &services.config),
        other if other.starts_with("includes.") => {
            includes::dispatch(other, params, &services.config)
        }
        other if other.starts_with("log.") => diagnostics::dispatch(other, params),
        other if other.starts_with("state.") => state::dispatch(other, params),
        other => handlers::dispatch(other, params),
    }
}

/// Chamadas que o painel repete sozinho, sem ação do usuário.
///
/// Só interessam quando falham — o sucesso é o caso comum e se repete a cada
/// poucos segundos.
fn is_polling(method: &str) -> bool {
    matches!(
        method,
        "server.ping"
            | "server.pidsOnPort"
            | "server.projectServersOnPort"
            | "server.readLog"
            // Registrar que se registrou dobra o log e não diz nada.
            | "log.write"
    )
}

/// Todos os métodos que este core entende: os sem estado e os da engine.
#[must_use]
pub fn method_names() -> Vec<&'static str> {
    let mut names = handlers::method_names();
    names.extend(compiler::METHODS);
    names.extend(config::METHODS);
    names.extend(debugger::METHODS);
    names.extend(server::METHODS);
    names.extend(engine::METHODS);
    names.extend(diagnostics::METHODS);
    names.extend(includes::METHODS);
    names.extend(state::METHODS);
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::mpsc;

    /// Coletor que guarda o que foi escrito, para inspecionar nos testes.
    struct Collector(mpsc::Sender<String>);

    impl Write for Collector {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let text = String::from_utf8_lossy(buf).into_owned();
            if !text.trim().is_empty() {
                let _ = self.0.send(text.trim().to_string());
            }
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Serviços com a configuração global numa pasta vazia: lendo o
    /// `~/.pawnpro` de quem roda os testes, eles dependeriam da máquina.
    fn services(sender: &Sender) -> Services {
        let home = std::env::temp_dir().join("pawnpro-rpc-home");
        let _ = std::fs::create_dir_all(&home);
        Services::with_config(sender, ConfigService::with_home(Some(home)))
    }

    /// Roda o laço com as linhas dadas e devolve o que saiu.
    fn exchange(input: &str) -> Vec<serde_json::Value> {
        let (tx, rx) = mpsc::channel();
        let sender = Sender::new(Box::new(Collector(tx)));
        // A engine não chega a subir: os testes só exercitam o laço.
        let services = services(&sender);
        serve(Cursor::new(input), &sender, &services).expect("laço");
        drop(services);
        drop(sender);
        rx.try_iter()
            .filter_map(|l| serde_json::from_str(&l).ok())
            .collect()
    }

    /// Compilar leva segundos. Se `compiler.run` rodasse no laço, todo pedido
    /// que chegasse nesse tempo esperaria o compilador — a sondagem do painel,
    /// o reinício da depuração.
    #[cfg(unix)]
    #[test]
    fn a_slow_compile_does_not_hold_other_requests() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("pawnpro-slow-compile-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("pasta");
        let fake = dir.join("pawncc");
        std::fs::write(&fake, "#!/bin/sh\nsleep 1\necho compilado\n").expect("compilador falso");
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        let (tx, rx) = mpsc::channel();
        let sender = Sender::new(Box::new(Collector(tx)));
        let services = services(&sender);
        services.config.open(&dir);
        let input = format!(
            "{}\n{}\n",
            json!({ "jsonrpc": "2.0", "id": 1, "method": "compiler.run",
                    "params": { "exe": fake, "args": [], "cwd": dir } }),
            json!({ "jsonrpc": "2.0", "id": 2, "method": "core.version" }),
        );
        serve(Cursor::new(input), &sender, &services).expect("laço");

        // Só as respostas: abrir o projeto também emite `config.changed`.
        let ids: Vec<serde_json::Value> = std::iter::from_fn(|| {
            let line = rx.recv_timeout(std::time::Duration::from_secs(10)).ok()?;
            serde_json::from_str::<serde_json::Value>(&line).ok()
        })
        .filter(|m| m.get("id").is_some_and(|id| !id.is_null()))
        .take(2)
        .map(|m| {
            if m["id"] == 1 {
                assert_eq!(m["result"]["output"], "compilado\n");
                assert_eq!(m["result"]["exitCode"], 0);
            }
            m["id"].clone()
        })
        .collect();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            ids,
            [json!(2), json!(1)],
            "o core.version esperou o compilador"
        );
    }

    #[test]
    fn answers_with_the_same_id_it_received() {
        let out = exchange(r#"{"jsonrpc":"2.0","id":7,"method":"core.version"}"#);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["id"], 7);
        assert!(out[0]["result"]["methods"].is_array());
    }

    #[test]
    fn a_text_id_comes_back_as_text() {
        // Um cliente que manda `"1"` espera `"1"`, não `1`.
        let out = exchange(r#"{"jsonrpc":"2.0","id":"abc","method":"core.version"}"#);
        assert_eq!(out[0]["id"], "abc");
    }

    #[test]
    fn an_unknown_method_is_an_error_not_a_crash() {
        let out = exchange(r#"{"jsonrpc":"2.0","id":1,"method":"nao.existe"}"#);
        assert_eq!(out[0]["error"]["code"], -32601);
    }

    #[test]
    fn missing_parameters_are_reported() {
        let out = exchange(r#"{"jsonrpc":"2.0","id":1,"method":"server.loadConfig"}"#);
        assert_eq!(out[0]["error"]["code"], -32602);
        assert!(out[0]["error"]["message"].as_str().unwrap().contains("cwd"));
    }

    #[test]
    fn a_notification_gets_no_answer() {
        // Sem `id` não há a quem responder.
        let out = exchange(r#"{"jsonrpc":"2.0","method":"core.version"}"#);
        assert!(out.is_empty());
    }

    #[test]
    fn a_malformed_line_does_not_stop_the_loop() {
        // Uma linha quebrada não pode derrubar o core: as seguintes continuam.
        let out = exchange(
            "{ isto não é json\n\
             \n\
             {\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"core.version\"}\n",
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["id"], 2);
    }

    #[test]
    fn several_requests_are_answered_in_order() {
        let out = exchange(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"core.version\"}\n\
             {\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"core.version\"}\n",
        );
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["id"], 1);
        assert_eq!(out[1]["id"], 2);
    }

    #[test]
    fn killing_a_process_that_is_not_ours_is_refused() {
        // A política de dono vive no core: a extensão não consegue contorná-la
        // pedindo direto por RPC.
        let out = exchange(
            r#"{"jsonrpc":"2.0","id":1,"method":"server.kill","params":{"pid":1,"exe":"/bin/sh"}}"#,
        );
        assert_eq!(out[0]["error"]["code"], -32000);
    }

    #[test]
    fn every_listed_method_is_dispatchable() {
        // Um nome na lista que o `dispatch` não conhece faria a extensão
        // chamar algo inexistente.
        let (tx, _rx) = mpsc::channel();
        let sender = Sender::new(Box::new(Collector(tx)));
        let services = services(&sender);
        for method in method_names() {
            // O mesmo caminho do laço: primeiro os de segundo plano.
            let err = background_job(method, &json!({}), &services).map_or_else(
                || handle(method, &json!({}), &sender, &services).err(),
                Result::err,
            );
            let code = err.map_or(0, |e| e.code);
            assert_ne!(code, -32601, "{method} está na lista mas não é despachado");
        }
    }

    #[test]
    fn starting_the_engine_twice_keeps_the_same_address() {
        // A extensão pode pedir o início por janela, sem coordenar entre elas.
        // A engine só sobe para um projeto: é ele que diz qual configuração
        // o core deve ler e entregar.
        let start = |id: u8| {
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "engine.start",
                "params": { "workspaceRoot": std::env::temp_dir(), "editorLanguage": "pt-br" }
            })
            .to_string()
        };
        let out = exchange(&format!("{}\n{}\n", start(1), start(2)));
        // Só as respostas: o supervisor notifica o estado da engine no mesmo
        // canal, e essas mensagens não têm `id`.
        let addresses: Vec<&str> = out
            .iter()
            .filter(|m| m.get("id").is_some())
            .filter_map(|m| m["result"]["address"].as_str())
            .collect();
        assert_eq!(addresses.len(), 2, "faltou resposta: {out:?}");
        assert!(!addresses[0].is_empty());
        assert_eq!(addresses[0], addresses[1]);
    }

    #[test]
    fn a_config_write_answers_with_the_new_value_and_notifies() {
        // Quem gravou recebe na resposta o que passou a valer; os demais
        // assinantes da extensão ficam sabendo pela notificação.
        let root = std::env::temp_dir().join(format!("pawnpro-rpc-cfg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("criar projeto");
        let line = |id: u8, method: &str, params: serde_json::Value| {
            json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string()
        };
        let out = exchange(&format!(
            "{}\n{}\n",
            line(1, "config.open", json!({ "workspaceRoot": root })),
            line(
                2,
                "config.set",
                json!({ "entries": [{ "key": "locale", "value": "ru" }], "scope": "project" })
            ),
        ));
        let _ = std::fs::remove_dir_all(&root);

        let answer = out.iter().find(|m| m["id"] == 2).expect("resposta ao set");
        assert_eq!(answer["result"]["config"]["locale"], "ru", "{answer}");
        let notified = out
            .iter()
            .filter(|m| m["method"] == "config.changed")
            .filter_map(|m| m["params"]["config"]["locale"].as_str())
            .collect::<Vec<_>>();
        assert_eq!(notified, ["", "ru"], "um aviso na abertura e um na escrita");
    }

    #[test]
    fn config_methods_refuse_before_a_project_opens() {
        let out = exchange(r#"{"jsonrpc":"2.0","id":1,"method":"config.reload"}"#);
        assert_eq!(out[0]["error"]["code"], -32602);
    }

    #[test]
    fn every_answer_uses_camel_case() {
        // A extensão lê `filePath`, não `file_path`. Um campo em snake_case
        // atravessa o RPC sem erro nenhum e chega como `undefined` do outro
        // lado — silencioso, e só percebido pelo recurso que parou de
        // funcionar. Aconteceu com o `NativeEntry`.
        fn walk(value: &serde_json::Value, path: &str) {
            match value {
                serde_json::Value::Object(map) => {
                    for (key, nested) in map {
                        assert!(!key.contains('_'), "`{path}.{key}` não está em camelCase");
                        walk(nested, &format!("{path}.{key}"));
                    }
                }
                serde_json::Value::Array(items) => {
                    for item in items {
                        walk(item, path);
                    }
                }
                _ => {}
            }
        }

        // Um include de verdade: com arquivo ausente a lista sai vazia e o
        // `NativeEntry` — justamente o que tinha o defeito — não é exercitado.
        // Pelo `handle`, o mesmo roteamento do `serve`: cada método é conferido
        // pelo caminho que a extensão usa, e o teste não quebra quando um
        // método muda de módulo.
        let (tx, _rx) = mpsc::channel();
        let sender = Sender::new(Box::new(Collector(tx)));
        let services = services(&sender);
        let call = |method: &str, params: &serde_json::Value| {
            handle(method, params, &sender, &services).expect(method)
        };

        let tmp = std::env::temp_dir();
        let inc = tmp.join(format!("pawnpro-rpc-{}.inc", std::process::id()));
        std::fs::write(&inc, "native MyNative(value);\n").expect("escrever include");
        let natives = call("includes.listNatives", &json!({ "file": &inc }));
        assert!(
            natives[0]["filePath"].is_string(),
            "o `NativeEntry` não veio em camelCase: {natives}"
        );

        let cases = [
            (
                "server.readLog",
                json!({ "path": tmp.join("nao-existe.log"), "from": 0 }),
            ),
            ("server.loadConfig", json!({ "cwd": tmp })),
            ("server.pidsOnPort", json!({ "port": 7777 })),
            ("debug.preflight", json!({ "cwd": tmp })),
            (
                "includes.listNatives",
                json!({ "file": tmp.join("nao-existe.inc") }),
            ),
        ];
        for (method, params) in cases {
            walk(&call(method, &params), method);
        }
    }
}

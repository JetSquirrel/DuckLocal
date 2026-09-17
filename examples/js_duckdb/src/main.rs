use std::path::PathBuf;

use duckdb::Connection;
use gpui_kit::{AppContext, Empty, WindowOptions};
use gpui_shell::{HostError, HostModule, HostObject, HostValue, ShellRuntime};

fn query_demo() -> duckdb::Result<HostValue> {
    let connection = Connection::open_in_memory()?;
    let mut statement = connection.prepare(
        "SELECT id, name FROM (VALUES (1, 'DuckDB'), (2, 'JavaScript'), (3, 'GPUI')) AS demo(id, name) ORDER BY id",
    )?;
    let mut rows = statement.query([])?;
    let mut result = Vec::new();
    while let Some(row) = rows.next()? {
        let id: i32 = row.get(0)?;
        let name: String = row.get(1)?;
        println!("{id} | {name}");
        result.push(HostValue::from(
            HostObject::new().field("id", id).field("name", name),
        ));
    }
    Ok(HostValue::Array(result))
}

fn demo_module() -> HostModule {
    HostModule::new("duckdb-demo")
        .declarations("export function queryDemo(): Promise<{ id: number; name: string }[]>;")
        .async_function("queryDemo", |arguments| {
            if !arguments.is_empty() {
                return Err(HostError::new("queryDemo takes no arguments"));
            }
            Ok(async { query_demo().map_err(|error| HostError::new(error.to_string())) })
        })
}

fn main() {
    tracing_subscriber::fmt::init();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui");
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            gpui_shell::init(cx);
            gpui_shell::export_module(demo_module()).expect("register demo query");
            let runtime = ShellRuntime::new(cx).expect("create JavaScript runtime");
            if std::env::args().any(|argument| argument == "--check") {
                let window = cx
                    .open_window(
                        WindowOptions {
                            show: false,
                            focus: false,
                            ..Default::default()
                        },
                        |_, cx| cx.new(|_| Empty),
                    )
                    .expect("open hidden check window");
                let result = window
                    .update(cx, |_, window, cx| runtime.check(&root, window, cx))
                    .expect("check window");
                match result {
                    Ok(_) => {
                        println!("JavaScript description and eager materialization passed");
                        std::process::exit(0);
                    }
                    Err(error) => {
                        eprintln!("{error:#}");
                        std::process::exit(1);
                    }
                }
            }
            cx.open_window(WindowOptions::default(), move |window, cx| {
                let root = runtime.load(&root, window, cx);
                match runtime.watch(&root, window, cx) {
                    Ok(watcher) => watcher.forget(),
                    Err(error) => eprintln!("Hot reload unavailable: {error}"),
                }
                root
            })
            .expect("open demo window");
            cx.on_window_closed(|cx, _| {
                gpui_shell::clear_exported_modules();
                cx.quit();
            })
            .detach();
            cx.activate(true);
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_shell::HostArguments;

    #[test]
    fn real_duckdb_rows_cross_the_plain_data_boundary() {
        let result = query_demo().unwrap();
        let rows = result.as_array().unwrap();
        assert_eq!(rows.len(), 3);
        for (row, (id, name)) in rows
            .iter()
            .zip([(1., "DuckDB"), (2., "JavaScript"), (3., "GPUI")])
        {
            assert_eq!(row.get("id").and_then(HostValue::as_number), Some(id));
            assert_eq!(row.get("name").and_then(HostValue::as_str), Some(name));
        }
    }

    #[test]
    fn module_exposes_only_the_fixed_async_query() {
        let module = demo_module();
        module.validate().unwrap();
        assert_eq!(module.function_names(), vec!["queryDemo"]);
        assert!(module.is_async("queryDemo"));
        assert!(module.begin("queryDemo", &HostArguments::default()).is_ok());
        for value in [
            HostValue::from("SELECT * FROM secrets"),
            HostValue::Null,
            HostValue::from(3),
        ] {
            assert!(
                module
                    .begin("queryDemo", &HostArguments::new([value]))
                    .is_err()
            );
        }
    }
}

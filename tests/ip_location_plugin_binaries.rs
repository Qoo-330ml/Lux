use std::{
    io::{BufRead, BufReader, Write},
    process::{Command, Stdio},
};

use serde_json::{Value, json};

#[test]
fn ip_location_plugin_binaries_implement_the_common_rpc() -> Result<(), Box<dyn std::error::Error>>
{
    for (binary, expected_id, expected_name) in [
        (
            env!("CARGO_BIN_EXE_lux-plugin-ip-hiofd"),
            "org.lux.ip-hiofd",
            "IP归属地查询增强",
        ),
        (
            env!("CARGO_BIN_EXE_lux-plugin-qoo-ip138"),
            "org.lux.qoo-ip138",
            "ip138 IP归属地查询",
        ),
    ] {
        let mut child = Command::new(binary)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let stdin = child.stdin.as_mut().ok_or("plugin stdin unavailable")?;
        for request in [
            json!({"id": "hello", "method": "plugin.hello", "params": {}}),
            json!({
                "id": "private",
                "method": "ip.location",
                "params": {"ip": "127.0.0.1"}
            }),
        ] {
            writeln!(stdin, "{}", serde_json::to_string(&request)?)?;
        }
        drop(child.stdin.take());

        let stdout = child.stdout.take().ok_or("plugin stdout unavailable")?;
        let mut lines = BufReader::new(stdout).lines();
        let hello: Value = serde_json::from_str(&lines.next().ok_or("missing hello")??)?;
        let invalid: Value =
            serde_json::from_str(&lines.next().ok_or("missing invalid response")??)?;
        assert_eq!(hello["id"], "hello");
        assert_eq!(hello["result"]["id"], expected_id);
        assert_eq!(hello["result"]["name"], expected_name);
        assert_eq!(hello["result"]["capabilities"], json!(["ip.location"]));
        assert_eq!(invalid["id"], "private");
        assert_eq!(invalid["error"]["code"], "IP_LOCATION_INVALID_REQUEST");
        assert!(child.wait()?.success());
    }
    Ok(())
}

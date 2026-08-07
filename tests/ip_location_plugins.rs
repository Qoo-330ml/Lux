use std::{fs, net::IpAddr};

use luxd::{
    application::{
        plugin_protocol::{
            IP_LOCATION_CAPABILITY, PLUGIN_CATEGORY_NETWORK, PLUGIN_TYPE_IP_LOCATION,
        },
        plugins::PluginService,
    },
    config::Config,
    storage::Database,
};
use serde_json::json;
use tempfile::tempdir;

#[cfg(unix)]
#[tokio::test]
async fn ip_location_query_falls_back_to_the_next_installed_plugin()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;

    let root = tempdir()?;
    let config_dir = root.path().join("config");
    let hiofd_dir = config_dir.join("plugins/org.lux.ip-hiofd");
    let qoo_dir = config_dir.join("plugins/org.lux.qoo-ip138");
    write_fake_plugin(
        &hiofd_dir,
        "org.lux.ip-hiofd",
        r#"{"ip":"1.1.1.1","country":"错误结果"}"#,
    )?;
    write_fake_plugin(
        &qoo_dir,
        "org.lux.qoo-ip138",
        r#"{"ip":"8.8.8.8","country":"美国","city":"山景城","isp":"Google"}"#,
    )?;
    for path in [
        hiofd_dir.join("binaries/plugin"),
        qoo_dir.join("binaries/plugin"),
    ] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }

    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: config_dir.clone(),
    };
    let database = Database::connect(&config).await?;
    let plugins = PluginService::new(database, config_dir);

    let result = plugins
        .lookup_ip_location("8.8.8.8".parse::<IpAddr>()?)
        .await?;

    assert_eq!(result.ip, "8.8.8.8");
    assert_eq!(result.city.as_deref(), Some("山景城"));
    assert_eq!(result.isp.as_deref(), Some("Google"));
    Ok(())
}

#[cfg(unix)]
fn write_fake_plugin(
    root: &std::path::Path,
    plugin_id: &str,
    location_result: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    fs::create_dir_all(root.join("binaries"))?;
    let script = r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
  method=$(printf '%s' "$line" | sed -n 's/.*"method":"\([^"]*\)".*/\1/p')
  if [ "$method" = "ip.location" ]; then
    printf '{"id":"%s","result":__RESULT__}\n' "$id"
  else
    printf '{"id":"%s","result":{"ok":true}}\n' "$id"
  fi
done
"#
    .replace("__RESULT__", location_result);
    fs::write(root.join("binaries/plugin"), script)?;
    fs::write(
        root.join("manifest.json"),
        serde_json::to_vec_pretty(&json!({
            "formatVersion": 1,
            "id": plugin_id,
            "name": plugin_id,
            "version": "1.0.0",
            "apiVersion": 1,
            "runtime": {"kind": "process", "entrypoint": "binaries/plugin"},
            "type": PLUGIN_TYPE_IP_LOCATION,
            "category": PLUGIN_CATEGORY_NETWORK,
            "capabilities": [IP_LOCATION_CAPABILITY],
            "permissions": {"network": ["example.invalid"]},
            "files": []
        }))?,
    )?;
    Ok(())
}

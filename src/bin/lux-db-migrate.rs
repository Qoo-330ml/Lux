use std::{env, path::PathBuf};

use luxd::storage::migration::{
    MigrationOptions, connection_from_environment, migrate_sqlite_to_postgres,
};

const DEFAULT_BATCH_SIZE: usize = 500;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let options = parse_args(env::args().skip(1))?;
    let connection = connection_from_environment(|name| env::var(name).ok())?;
    let report = migrate_sqlite_to_postgres(&options, &connection).await?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<MigrationOptions, String> {
    let Some(command) = args.next() else {
        return Err(usage());
    };
    if command != "sqlite-to-postgres" {
        return Err(usage());
    }
    let mut source = None;
    let mut batch_size = DEFAULT_BATCH_SIZE;
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--source" => source = Some(PathBuf::from(args.next().ok_or_else(usage)?)),
            "--batch-size" => {
                batch_size = args
                    .next()
                    .ok_or_else(usage)?
                    .parse()
                    .map_err(|_| usage())?;
            }
            "--password" | "--database-url" => {
                return Err("database credentials are accepted only through LUX_MIGRATE_POSTGRES_* environment variables".to_owned());
            }
            _ => return Err(usage()),
        }
    }
    let source = source.ok_or_else(usage)?;
    MigrationOptions::new(source, batch_size).map_err(|error| error.to_string())
}

fn usage() -> String {
    "usage: lux-db-migrate sqlite-to-postgres --source /config/lux.db [--batch-size 500]".to_owned()
}

#[cfg(test)]
mod tests {
    use super::parse_args;

    #[test]
    fn parses_source_and_batch_size() {
        let options = parse_args(
            [
                "sqlite-to-postgres",
                "--source",
                "/config/lux.db",
                "--batch-size",
                "250",
            ]
            .into_iter()
            .map(str::to_owned),
        )
        .unwrap();
        assert_eq!(options.source.to_string_lossy(), "/config/lux.db");
        assert_eq!(options.batch_size, 250);
    }

    #[test]
    fn rejects_credentials_on_the_command_line() {
        for argument in ["--password", "--database-url"] {
            let error = parse_args(
                [
                    "sqlite-to-postgres",
                    "--source",
                    "/config/lux.db",
                    argument,
                    "secret",
                ]
                .into_iter()
                .map(str::to_owned),
            )
            .unwrap_err();
            assert!(error.contains("environment variables"));
            assert!(!error.contains("secret"));
        }
    }
}

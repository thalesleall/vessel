use tracing_subscriber::EnvFilter;

const USAGE: &str = "usage:
  vessel-agent                          serve (configured by VESSEL_* variables)
  vessel-agent backup <file>            encrypted copy of VESSEL_DATABASE
  vessel-agent restore <file> <db>      decrypt a backup into a new database file
backup and restore read the passphrase from VESSEL_BACKUP_PASSPHRASE";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let database = || {
        std::path::PathBuf::from(
            std::env::var("VESSEL_DATABASE").unwrap_or_else(|_| "/data/vessel.db".into()),
        )
    };
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        [] => {}
        ["backup", out] => {
            vessel_agent::backup::backup(&database(), std::path::Path::new(out))?;
            println!("encrypted backup written to {out}");
            return Ok(());
        }
        ["restore", file, db] => {
            vessel_agent::backup::restore(std::path::Path::new(file), std::path::Path::new(db))?;
            println!("restored into {db}");
            return Ok(());
        }
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    }

    let config = vessel_agent::Config::from_env()?;
    let listener = tokio::net::TcpListener::bind(&config.bind).await?;
    tracing::info!(bind = %config.bind, public_url = %config.public_url, signups = ?config.signups, "vessel agent starting");
    vessel_agent::serve(config, listener).await
}

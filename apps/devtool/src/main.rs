//! `vessel-dev`: a scriptable second Vessel user, for testing sync and team sharing by hand.
//!
//! State lives in a local SQLite file (`--db`, default `deploy/dev/.lab/bot.db`); the
//! password comes from `--password` or `VESSEL_DEV_PASSWORD`. Not for real accounts.
//!
//! ```text
//! vessel-dev signup <server-url> <email>     create the account (prints the recovery key)
//! vessel-dev login  <server-url> <email>     sign this db in to an existing account
//! vessel-dev status | fingerprint | sync
//! vessel-dev vaults                          list vaults and roles
//! vessel-dev create-vault <name>
//! vessel-dev invite <vault> <viewer|editor|admin> [email]   prints an invite link
//! vessel-dev accept <invite-link-or-token>
//! vessel-dev members <vault>
//! vessel-dev approve <vault> <email>         approve a pending member
//! vessel-dev role <vault> <email> <role>
//! vessel-dev remove <vault> <email>
//! vessel-dev leave <vault>
//! vessel-dev hosts                           list hosts the bot can see (secrets hidden)
//! vessel-dev add-host <vault> <label> <address> [port]
//! vessel-dev rename-host <label> <new-label>
//! ```
//! `<vault>` is a vault name or id.

use std::error::Error;
use std::path::PathBuf;

use uuid::Uuid;
use vessel_client::model::{Entity, EntityKind, Host};
use vessel_client::{Role, Vessel};

type R<T> = Result<T, Box<dyn Error>>;

struct Args {
    db: PathBuf,
    password: Option<String>,
    rest: Vec<String>,
}

fn parse_args() -> Args {
    let mut db = PathBuf::from("deploy/dev/.lab/bot.db");
    let mut password = std::env::var("VESSEL_DEV_PASSWORD").ok();
    let mut rest = Vec::new();
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--db" => db = it.next().map(PathBuf::from).unwrap_or(db),
            "--password" => password = it.next(),
            _ => rest.push(a),
        }
    }
    Args { db, password, rest }
}

fn role(s: &str) -> R<Role> {
    Ok(match s {
        "viewer" => Role::Viewer,
        "editor" => Role::Editor,
        "admin" => Role::Admin,
        "owner" => Role::Owner,
        _ => return Err(format!("unknown role {s:?} (viewer, editor, admin)").into()),
    })
}

fn vault_id(v: &Vessel, name: &str) -> R<Uuid> {
    let vaults = v.vaults()?;
    vaults
        .iter()
        .find(|x| x.id.to_string() == name || x.name.eq_ignore_ascii_case(name))
        .map(|x| x.id)
        .ok_or_else(|| format!("no vault named {name:?}").into())
}

async fn member_id(v: &mut Vessel, vault: Uuid, email: &str) -> R<Uuid> {
    let m = v.members(vault).await?;
    m.members
        .iter()
        .map(|x| (x.user_id, &x.email))
        .chain(m.pending.iter().map(|x| (x.user_id, &x.email)))
        .find(|(_, e)| e.eq_ignore_ascii_case(email))
        .map(|(id, _)| id)
        .ok_or_else(|| format!("{email} is not a member or pending in that vault").into())
}

fn arg(rest: &[String], i: usize, what: &str) -> R<String> {
    rest.get(i)
        .cloned()
        .ok_or_else(|| format!("missing <{what}>").into())
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("vessel-dev: {e}");
        std::process::exit(1);
    }
}

async fn run() -> R<()> {
    let args = parse_args();
    let Some(cmd) = args.rest.first().cloned() else {
        println!(
            "{}",
            include_str!("main.rs")
                .lines()
                .take(22)
                .collect::<Vec<_>>()
                .join("\n")
        );
        return Ok(());
    };
    let rest = &args.rest[1..];
    if let Some(dir) = args.db.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut v = Vessel::open(&args.db)?;
    let password = || {
        args.password
            .clone()
            .ok_or("set VESSEL_DEV_PASSWORD or pass --password")
    };

    match cmd.as_str() {
        "signup" => {
            let recovery = v
                .signup(
                    &arg(rest, 0, "server-url")?,
                    &arg(rest, 1, "email")?,
                    &password()?,
                    "vessel-dev",
                    None,
                )
                .await?;
            println!("conta criada; chave de recuperação: {recovery}");
            println!("impressão digital: {}", v.fingerprint()?);
            return Ok(());
        }
        "login" => {
            v.login(
                &arg(rest, 0, "server-url")?,
                &arg(rest, 1, "email")?,
                &password()?,
                "vessel-dev",
            )
            .await?;
            println!("conectado");
            return Ok(());
        }
        _ => {}
    }

    if !v.status().signed_in {
        return Err("this db is not signed in; run `signup` or `login` first".into());
    }
    v.unlock(&password()?)?;
    // Every command works on fresh data.
    v.sync().await?;

    match cmd.as_str() {
        "status" => println!("{}", serde_json::to_string_pretty(&v.status())?),
        "fingerprint" => println!("{}", v.fingerprint()?),
        "sync" => {
            let r = v.sync().await?;
            println!(
                "vaults={} pulled={} pushed={} conflicts={}",
                r.vaults, r.pulled, r.pushed, r.conflicts
            );
        }
        "vaults" => {
            for x in v.vaults()? {
                println!(
                    "{}  {:<20} {:?}  membros={}{}",
                    x.id,
                    x.name,
                    x.role,
                    x.member_count,
                    if x.personal { "  (pessoal)" } else { "" }
                );
            }
        }
        "create-vault" => println!("{}", v.create_vault(&arg(rest, 0, "name")?).await?),
        "invite" => {
            let id = vault_id(&v, &arg(rest, 0, "vault")?)?;
            let inv = v
                .invite(id, role(&arg(rest, 1, "role")?)?, rest.get(2).cloned())
                .await?;
            println!("{}", inv.url);
        }
        "accept" => {
            let vault = v.accept_invite(&arg(rest, 0, "invite")?).await?;
            println!("convite aceito (cofre {vault:?}); aguardando aprovação de um admin.");
            println!(
                "confira esta impressão digital com quem aprovar: {}",
                v.fingerprint()?
            );
        }
        "members" => {
            let id = vault_id(&v, &arg(rest, 0, "vault")?)?;
            let m = v.members(id).await?;
            for x in m.members {
                println!("membro   {:<30} {:?}  {}", x.email, x.role, x.fingerprint);
            }
            for x in m.pending {
                println!("pendente {:<30} {:?}  {}", x.email, x.role, x.fingerprint);
            }
        }
        "approve" => {
            let id = vault_id(&v, &arg(rest, 0, "vault")?)?;
            let user = member_id(&mut v, id, &arg(rest, 1, "email")?).await?;
            v.approve_member(id, user).await?;
            println!("aprovado");
        }
        "role" => {
            let id = vault_id(&v, &arg(rest, 0, "vault")?)?;
            let user = member_id(&mut v, id, &arg(rest, 1, "email")?).await?;
            v.change_role(id, user, role(&arg(rest, 2, "role")?)?)
                .await?;
            println!("papel alterado");
        }
        "remove" => {
            let id = vault_id(&v, &arg(rest, 0, "vault")?)?;
            let user = member_id(&mut v, id, &arg(rest, 1, "email")?).await?;
            v.remove_member(id, user).await?;
            println!("removido (a chave do cofre foi trocada)");
        }
        "leave" => {
            let id = vault_id(&v, &arg(rest, 0, "vault")?)?;
            v.leave_vault(id).await?;
            println!("saiu do cofre");
        }
        "hosts" => {
            let names: std::collections::HashMap<Uuid, String> =
                v.vaults()?.into_iter().map(|x| (x.id, x.name)).collect();
            for item in v.items(Some(EntityKind::Host))? {
                if let Entity::Host(h) = item.entity {
                    println!(
                        "{:<20} {}:{}  cofre={}  senha={}",
                        h.label,
                        h.address,
                        h.port.unwrap_or(22),
                        names.get(&item.vault_id).map_or("?", |s| s),
                        if h.password.is_some() {
                            "definida"
                        } else {
                            "-"
                        }
                    );
                }
            }
        }
        "add-host" => {
            let id = vault_id(&v, &arg(rest, 0, "vault")?)?;
            let host = Host {
                label: arg(rest, 1, "label")?,
                address: arg(rest, 2, "address")?,
                port: rest.get(3).map(|p| p.parse()).transpose()?,
                ..Host::default()
            };
            let item = v.save(id, None, Entity::Host(host))?;
            v.sync().await?;
            println!("host criado: {item}");
        }
        "rename-host" => {
            let label = arg(rest, 0, "label")?;
            let item = v
                .items(Some(EntityKind::Host))?
                .into_iter()
                .find(|i| matches!(&i.entity, Entity::Host(h) if h.label == label))
                .ok_or_else(|| format!("no host labelled {label:?}"))?;
            let Entity::Host(mut h) = item.entity else {
                unreachable!()
            };
            h.label = arg(rest, 1, "new-label")?;
            v.save(item.vault_id, Some(item.id), Entity::Host(h))?;
            v.sync().await?;
            println!("renomeado");
        }
        other => {
            return Err(format!("unknown command {other:?}; run without arguments for help").into());
        }
    }
    Ok(())
}

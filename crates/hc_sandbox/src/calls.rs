//! Helpers for making [`AdminRequest`]s to the admin api.
//!
//! This module is designed for use in a CLI so it is more simplified
//! then calling the [`CmdRunner`] directly.
//! For simple calls like [`AdminRequest::ListDnas`] this is probably easier
//! but if you want more control use [`CmdRunner::command`].
use std::path::Path;
use std::path::PathBuf;
use std::{collections::HashSet, convert::TryInto};

use anyhow::anyhow;
use anyhow::bail;
use anyhow::ensure;
use holochain_conductor_api::AdminInterfaceConfig;
use holochain_conductor_api::AdminRequest;
use holochain_conductor_api::AdminResponse;
use holochain_conductor_api::InterfaceDriver;
use holochain_p2p::kitsune_p2p;
use holochain_p2p::kitsune_p2p::agent_store::AgentInfoSigned;
use holochain_types::prelude::AgentPubKey;
use holochain_types::prelude::CellId;
use holochain_types::prelude::DnaHash;
use holochain_types::prelude::InstallAppDnaPayload;
use holochain_types::prelude::InstallAppPayload;
use holochain_types::prelude::InstalledCell;
use portpicker::is_free;
use std::convert::TryFrom;

use crate::cmds::Existing;
use crate::expect_match;
use crate::ports::get_admin_ports;
use crate::run::run_async;
use crate::CmdRunner;
use structopt::StructOpt;

#[doc(hidden)]
#[derive(Debug, StructOpt)]
pub struct Call {
    #[structopt(short, long, conflicts_with_all = &["existing_paths", "existing_indices"], value_delimiter = ",")]
    /// Ports to running conductor admin interfaces.
    /// If this is empty existing setups will be used.
    /// Cannot be combined with existing setups.
    pub running: Vec<u16>,
    #[structopt(flatten)]
    pub existing: Existing,
    #[structopt(subcommand)]
    /// The admin request you want to make.
    pub call: AdminRequestCli,
}

// Docs have different use for structopt
// so documenting everything doesn't make sense.
#[allow(missing_docs)]
#[derive(Debug, StructOpt, Clone)]
pub enum AdminRequestCli {
    AddAdminWs(AddAdminWs),
    AddAppWs(AddAppWs),
    InstallApp(InstallApp),
    /// Calls AdminRequest::ListDnas.
    ListDnas,
    /// Calls AdminRequest::GenerateAgentPubKey.
    NewAgent,
    /// Calls AdminRequest::ListCellIds.
    ListCells,
    /// Calls AdminRequest::ListActiveApps.
    ListActiveApps,
    ActivateApp(ActivateApp),
    DeactivateApp(DeactivateApp),
    DumpState(DumpState),
    /// Calls AdminRequest::AddAgentInfo.
    /// [Unimplemented].
    AddAgents,
    ListAgents(ListAgents),
}
#[derive(Debug, StructOpt, Clone)]
/// Calls AdminRequest::AddAdminInterfaces
/// and adds another admin interface.
pub struct AddAdminWs {
    /// Optional port number.
    /// Defaults to assigned by OS.
    pub port: Option<u16>,
}

#[derive(Debug, StructOpt, Clone)]
/// Calls AdminRequest::AttachAppInterface
/// and adds another app interface.
pub struct AddAppWs {
    /// Optional port number.
    /// Defaults to assigned by OS.
    pub port: Option<u16>,
}

#[derive(Debug, StructOpt, Clone)]
/// Calls AdminRequest::InstallApp
/// and installs a new app.
///
/// Setting properties and membrane proofs is not
/// yet supported.
/// CellNicks are set to `my-app-0`, `my-app-1` etc.
pub struct InstallApp {
    #[structopt(short, long, default_value = "test-app")]
    /// Sets the InstalledAppId.
    pub app_id: String,
    #[structopt(short, long, parse(try_from_str = parse_agent_key))]
    /// If not set then a key will be generated.
    /// Agent key is Base64 (same format that is used in logs).
    /// e.g. `uhCAk71wNXTv7lstvi4PfUr_JDvxLucF9WzUgWPNIEZIoPGMF4b_o`
    pub agent_key: Option<AgentPubKey>,
    #[structopt(required = true, min_values = 1)]
    /// List of dnas to install.
    pub dnas: Vec<PathBuf>,
}

#[derive(Debug, StructOpt, Clone)]
/// Calls AdminRequest::ActivateApp
/// and activates the installed app.
pub struct ActivateApp {
    /// The InstalledAppId to activate.
    pub app_id: String,
}

#[derive(Debug, StructOpt, Clone)]
/// Calls AdminRequest::DeactivateApp
/// and deactivates the installed app.
pub struct DeactivateApp {
    /// The InstalledAppId to deactivate.
    pub app_id: String,
}

#[derive(Debug, StructOpt, Clone)]
/// Calls AdminRequest::DumpState
/// and dumps the current cell's state.
/// TODO: Add pretty print.
/// TODO: Default to dumping all cell state.
pub struct DumpState {
    #[structopt(parse(try_from_str = parse_dna_hash))]
    /// The dna hash half of the cell id to dump.
    pub dna: DnaHash,
    #[structopt(parse(try_from_str = parse_agent_key))]
    /// The agent half of the cell id to dump.
    pub agent_key: AgentPubKey,
}
#[derive(Debug, StructOpt, Clone)]
/// Calls AdminRequest::RequestAgentInfo
/// and pretty prints the agent info on
/// this conductor.
pub struct ListAgents {
    #[structopt(short, long, parse(try_from_str = parse_agent_key), requires = "dna")]
    /// Optionally request agent info for a particular cell id.
    pub agent_key: Option<AgentPubKey>,
    #[structopt(short, long, parse(try_from_str = parse_dna_hash), requires = "agent_key")]
    /// Optionally request agent info for a particular cell id.
    pub dna: Option<DnaHash>,
}

#[doc(hidden)]
pub async fn call(holochain_path: &Path, req: Call) -> anyhow::Result<()> {
    let Call {
        existing,
        running,
        call,
    } = req;
    let cmds = if running.is_empty() {
        let paths = if existing.is_empty() {
            crate::save::load(std::env::current_dir()?)?
        } else {
            existing.load()?
        };
        let ports = get_admin_ports(paths.clone()).await?;
        let mut cmds = Vec::with_capacity(ports.len());
        for (port, path) in ports.into_iter().zip(paths.into_iter()) {
            match CmdRunner::try_new(port).await {
                Ok(cmd) => cmds.push((cmd, None)),
                Err(e) => match e.kind() {
                    std::io::ErrorKind::ConnectionRefused => {
                        let (port, holochain) = run_async(holochain_path, path, None).await?;
                        cmds.push((CmdRunner::new(port).await, Some(holochain)))
                    }
                    _ => bail!(
                        "Failed to connect to running conductor or start one {:?}",
                        e
                    ),
                },
            }
        }
        cmds
    } else {
        let mut cmds = Vec::with_capacity(running.len());
        for port in running {
            cmds.push((CmdRunner::new(port).await, None));
        }
        cmds
    };
    for mut cmd in cmds {
        call_inner(&mut cmd.0, call.clone()).await?;
    }
    Ok(())
}

async fn call_inner(cmd: &mut CmdRunner, call: AdminRequestCli) -> anyhow::Result<()> {
    match call {
        AdminRequestCli::AddAdminWs(args) => {
            let port = add_admin_interface(cmd, args).await?;
            msg!("Added Admin port {}", port);
        }
        AdminRequestCli::AddAppWs(args) => {
            let port = attach_app_interface(cmd, args).await?;
            msg!("Added App port {}", port);
        }
        AdminRequestCli::InstallApp(args) => {
            let app_id = args.app_id.clone();
            let cells = install_app(cmd, args).await?;
            msg!("Installed App: {} with cells {:?}", app_id, cells);
        }
        AdminRequestCli::ListDnas => {
            let dnas = list_dnas(cmd).await?;
            msg!("Dnas: {:?}", dnas);
        }
        AdminRequestCli::NewAgent => {
            let agent = generate_agent_pub_key(cmd).await?;
            msg!("Added agent {}", agent);
        }
        AdminRequestCli::ListCells => {
            let cells = list_cell_ids(cmd).await?;
            msg!("Cell Ids: {:?}", cells);
        }
        AdminRequestCli::ListActiveApps => {
            let apps = list_active_apps(cmd).await?;
            msg!("Active Apps: {:?}", apps);
        }
        AdminRequestCli::ActivateApp(args) => {
            let app_id = args.app_id.clone();
            activate_app(cmd, args).await?;
            msg!("Activated app: {:?}", app_id);
        }
        AdminRequestCli::DeactivateApp(args) => {
            let app_id = args.app_id.clone();
            deactivate_app(cmd, args).await?;
            msg!("Deactivated app: {:?}", app_id);
        }
        AdminRequestCli::DumpState(args) => {
            let state = dump_state(cmd, args).await?;
            msg!("DUMP STATE \n{}", state);
        }
        AdminRequestCli::AddAgents => todo!("Adding agent info via cli is coming soon"),
        AdminRequestCli::ListAgents(args) => {
            use std::fmt::Write;
            let agent_infos = request_agent_info(cmd, args).await?;
            for info in agent_infos {
                let mut out = String::new();
                let cell_info = list_cell_ids(cmd).await?;
                let agents = cell_info
                    .iter()
                    .map(|c| c.agent_pubkey().clone())
                    .map(|a| (a.clone(), holochain_p2p::agent_holo_to_kit(a)))
                    .collect::<Vec<_>>();

                let dnas = cell_info
                    .iter()
                    .map(|c| c.dna_hash().clone())
                    .map(|d| (d.clone(), holochain_p2p::space_holo_to_kit(d)))
                    .collect::<Vec<_>>();

                let info: kitsune_p2p::agent_store::AgentInfo = (&info).try_into().unwrap();
                let this_agent = agents.iter().find(|a| *info.as_agent_ref() == a.1).unwrap();
                let this_dna = dnas.iter().find(|d| *info.as_space_ref() == d.1).unwrap();
                writeln!(out, "This Agent {:?} is {:?}", this_agent.0, this_agent.1)?;
                writeln!(out, "This DNA {:?} is {:?}", this_dna.0, this_dna.1)?;

                use chrono::{DateTime, Duration, NaiveDateTime, Utc};
                let duration = Duration::milliseconds(info.signed_at_ms() as i64);
                let s = duration.num_seconds() as i64;
                let n = duration.clone().to_std().unwrap().subsec_nanos();
                let dt = DateTime::<Utc>::from_utc(NaiveDateTime::from_timestamp(s, n), Utc);
                let exp = dt + Duration::milliseconds(info.expires_after_ms() as i64);
                let now = Utc::now();

                writeln!(out, "signed at {}", dt)?;
                writeln!(
                    out,
                    "expires at {} in {}mins",
                    exp,
                    (exp - now).num_minutes()
                )?;
                writeln!(out, "space: {:?}", info.as_space_ref())?;
                writeln!(out, "agent: {:?}", info.as_agent_ref())?;
                writeln!(out, "urls: {:?}", info.as_urls_ref())?;
                msg!("{}\n", out);
            }
        }
    }
    Ok(())
}

/// Calls [`AdminRequest::AddAdminInterfaces`] and adds another admin interface.
pub async fn add_admin_interface(cmd: &mut CmdRunner, args: AddAdminWs) -> anyhow::Result<u16> {
    let port = match args.port {
        Some(port) => {
            ensure!(is_free(port), "port {} is not free", port);
            port
        }
        None => 0,
    };
    let resp = cmd
        .command(AdminRequest::AddAdminInterfaces(vec![
            AdminInterfaceConfig {
                driver: InterfaceDriver::Websocket { port },
            },
        ]))
        .await?;
    ensure!(
        matches!(resp, AdminResponse::AdminInterfacesAdded),
        "Failed to add admin interface, got: {:?}",
        resp
    );
    // TODO: return chosen port when 0 is used
    Ok(port)
}

/// Calls [`AdminRequest::InstallApp`] and installs a new app.
/// Creates an app per dna with the app id of `{app-id}-{dna-index}`
/// e.g. `my-cool-app-3`.
pub async fn install_app(
    cmd: &mut CmdRunner,
    args: InstallApp,
) -> anyhow::Result<HashSet<InstalledCell>> {
    let InstallApp {
        app_id,
        agent_key,
        dnas,
    } = args;
    let agent_key = match agent_key {
        Some(agent) => agent,
        None => generate_agent_pub_key(cmd).await?,
    };

    for path in &dnas {
        ensure!(path.is_file(), "Dna path {} must be a file", path.display());
    }

    // Turn dnas into payloads
    let dnas = dnas
        .into_iter()
        .enumerate()
        .map(|(i, path)| InstallAppDnaPayload::path_only(path, format!("{}-{}", app_id, i)))
        .collect::<Vec<_>>();

    let app = InstallAppPayload {
        installed_app_id: app_id,
        agent_key,
        dnas,
    };

    let r = AdminRequest::InstallApp(app.into());
    let installed_app = cmd.command(r).await?;
    let installed_app =
        expect_match!(installed_app => AdminResponse::AppInstalled, "Failed to install app");
    activate_app(
        cmd,
        ActivateApp {
            app_id: installed_app.installed_app_id.clone(),
        },
    )
    .await?;
    Ok(installed_app
        .cell_data
        .into_iter()
        // .map(|(n, c)| InstalledCell::new(c.clone(), n.clone()))
        .collect())
}

/// Calls [`AdminRequest::ListCellIds`].
pub async fn list_dnas(cmd: &mut CmdRunner) -> anyhow::Result<Vec<DnaHash>> {
    let resp = cmd.command(AdminRequest::ListDnas).await?;
    Ok(expect_match!(resp => AdminResponse::DnasListed, "Failed to list dnas"))
}

/// Calls [`AdminRequest::GenerateAgentPubKey`].
pub async fn generate_agent_pub_key(cmd: &mut CmdRunner) -> anyhow::Result<AgentPubKey> {
    let resp = cmd.command(AdminRequest::GenerateAgentPubKey).await?;
    Ok(
        expect_match!(resp => AdminResponse::AgentPubKeyGenerated, "Failed to generate agent pubkey"),
    )
}

/// Calls [`AdminRequest::ListCellIds`].
pub async fn list_cell_ids(cmd: &mut CmdRunner) -> anyhow::Result<Vec<CellId>> {
    let resp = cmd.command(AdminRequest::ListCellIds).await?;
    Ok(expect_match!(resp => AdminResponse::CellIdsListed, "Failed to list cell ids"))
}

/// Calls [`AdminRequest::ListActiveApps`].
pub async fn list_active_apps(cmd: &mut CmdRunner) -> anyhow::Result<Vec<String>> {
    let resp = cmd.command(AdminRequest::ListActiveApps).await?;
    Ok(expect_match!(resp => AdminResponse::ActiveAppsListed, "Failed to list active apps"))
}

/// Calls [`AdminRequest::ActivateApp`] and activates the installed app.
pub async fn activate_app(cmd: &mut CmdRunner, args: ActivateApp) -> anyhow::Result<()> {
    let resp = cmd
        .command(AdminRequest::ActivateApp {
            installed_app_id: args.app_id,
        })
        .await?;
    ensure!(
        matches!(resp, AdminResponse::AppActivated),
        "Failed to activate app, got: {:?}",
        resp
    );
    Ok(())
}

/// Calls [`AdminRequest::DeactivateApp`] and deactivates the installed app.
pub async fn deactivate_app(cmd: &mut CmdRunner, args: DeactivateApp) -> anyhow::Result<()> {
    let resp = cmd
        .command(AdminRequest::DeactivateApp {
            installed_app_id: args.app_id,
        })
        .await?;
    ensure!(
        matches!(resp, AdminResponse::AppDeactivated),
        "Failed to deactivate app, got: {:?}",
        resp
    );
    Ok(())
}

/// Calls [`AdminRequest::AttachAppInterface`] and adds another app interface.
pub async fn attach_app_interface(cmd: &mut CmdRunner, args: AddAppWs) -> anyhow::Result<u16> {
    if let Some(port) = args.port {
        ensure!(is_free(port), "port {} is not free", port);
    }
    let resp = cmd
        .command(AdminRequest::AttachAppInterface { port: args.port })
        .await?;
    match resp {
        AdminResponse::AppInterfaceAttached { port } => Ok(port),
        _ => Err(anyhow!(
            "Failed to attach app interface {:?}, got: {:?}",
            args.port,
            resp
        )),
    }
}

/// Calls [`AdminRequest::DumpState`] and dumps the current cell's state.
// TODO: Add pretty print.
// TODO: Default to dumping all cell state.
pub async fn dump_state(cmd: &mut CmdRunner, args: DumpState) -> anyhow::Result<String> {
    let resp = cmd
        .command(AdminRequest::DumpState {
            cell_id: Box::new(args.into()),
        })
        .await?;
    Ok(expect_match!(resp => AdminResponse::StateDumped, "Failed to dump state"))
}

/// Calls [`AdminRequest::AddAgentInfo`] with and adds the list of agent info.
pub async fn add_agent_info(cmd: &mut CmdRunner, args: Vec<AgentInfoSigned>) -> anyhow::Result<()> {
    let resp = cmd
        .command(AdminRequest::AddAgentInfo { agent_infos: args })
        .await?;
    ensure!(
        matches!(resp, AdminResponse::AgentInfoAdded),
        "Failed to add agent info, got: {:?}",
        resp
    );
    Ok(())
}

/// Calls [`AdminRequest::RequestAgentInfo`] and pretty prints the agent info on this conductor.
pub async fn request_agent_info(
    cmd: &mut CmdRunner,
    args: ListAgents,
) -> anyhow::Result<Vec<AgentInfoSigned>> {
    let resp = cmd
        .command(AdminRequest::RequestAgentInfo {
            cell_id: args.into(),
        })
        .await?;
    Ok(expect_match!(resp => AdminResponse::AgentInfoRequested, "Failed to request agent info"))
}

fn parse_agent_key(arg: &str) -> anyhow::Result<AgentPubKey> {
    AgentPubKey::try_from(arg).map_err(|e| anyhow::anyhow!("{:?}", e))
}

fn parse_dna_hash(arg: &str) -> anyhow::Result<DnaHash> {
    DnaHash::try_from(arg).map_err(|e| anyhow::anyhow!("{:?}", e))
}

impl From<CellId> for DumpState {
    fn from(cell_id: CellId) -> Self {
        let (dna, agent_key) = cell_id.into_dna_and_agent();
        Self { agent_key, dna }
    }
}

impl From<DumpState> for CellId {
    fn from(ds: DumpState) -> Self {
        CellId::new(ds.dna, ds.agent_key)
    }
}

impl From<ListAgents> for Option<CellId> {
    fn from(la: ListAgents) -> Self {
        let ListAgents {
            agent_key: a,
            dna: d,
        } = la;
        d.and_then(|d| a.map(|a| (d, a)))
            .map(|(d, a)| CellId::new(d, a))
    }
}
use holochain_p2p::kitsune_p2p::KitsuneP2pConfig;
use holochain_p2p::kitsune_p2p::TransportConfig;
use holochain_types::prelude::InstalledAppId;
use structopt::StructOpt;
use url2::Url2;

const DEFAULT_APP_ID: &'static str = "test-app";
#[derive(Debug, StructOpt, Clone)]
pub struct Create {
    #[structopt(subcommand)]
    /// Add an optional network.
    pub network: Option<NetworkCmd>,
    #[structopt(short, long, default_value = DEFAULT_APP_ID)]
    /// Id for the installed app.
    /// This is just a String to identify the app by.
    pub app_id: InstalledAppId,
}

#[derive(Debug, StructOpt, Clone)]
pub enum NetworkCmd {
    Network(Network),
}

impl NetworkCmd {
    pub fn into_inner(self) -> Network {
        match self {
            NetworkCmd::Network(n) => n,
        }
    }
}

#[derive(Debug, StructOpt, Clone)]
pub struct Network {
    #[structopt(subcommand)]
    /// Set the type of network.
    pub transport: NetworkType,
    #[structopt(short, long, parse(from_str = Url2::parse))]
    /// Optionally set a bootstrap url.
    /// The service used for peers to discover each before they are peers.
    pub bootstrap: Option<Url2>,
}

#[derive(Debug, StructOpt, Clone)]
pub enum NetworkType {
    /// A transport that uses the local memory transport protocol.
    Mem,
    /// A transport that uses the QUIC protocol.
    Quic(Quic),
}

#[derive(Debug, StructOpt, Clone)]
pub struct Quic {
    #[structopt(short, parse(from_str = Url2::parse))]
    /// To which network interface / port should we bind?
    /// Default: "kitsune-quic://0.0.0.0:0".
    pub bind_to: Option<Url2>,
    /// If you have port-forwarding set up,
    /// or wish to apply a vanity domain name,
    /// you may need to override the local NIC ip.
    /// Default: None = use NIC ip.
    pub override_host: Option<String>,
    #[structopt(short)]
    /// If you have port-forwarding set up,
    /// you may need to override the local NIC port.
    /// Default: None = use NIC port.
    pub override_port: Option<u16>,
    #[structopt(short, parse(from_str = Url2::parse))]
    /// Run through an external proxy at this url.
    pub proxy: Option<Url2>,
}

impl From<Network> for KitsuneP2pConfig {
    fn from(n: Network) -> Self {
        let Network {
            transport,
            bootstrap,
        } = n;
        let mut kit = KitsuneP2pConfig::default();
        kit.bootstrap_service = bootstrap;

        match transport {
            NetworkType::Mem => (),
            NetworkType::Quic(Quic {
                bind_to,
                override_host,
                override_port,
                proxy: None,
            }) => {
                kit.transport_pool = vec![TransportConfig::Quic {
                    bind_to,
                    override_host,
                    override_port,
                }];
            }
            NetworkType::Quic(Quic {
                bind_to,
                override_host,
                override_port,
                proxy: Some(proxy_url),
            }) => {
                let transport = TransportConfig::Quic {
                    bind_to,
                    override_host,
                    override_port,
                };
                kit.transport_pool = vec![TransportConfig::Proxy {
                    sub_transport: Box::new(transport),
                    proxy_config: holochain_p2p::kitsune_p2p::ProxyConfig::RemoteProxyClient {
                        proxy_url,
                    },
                }]
            }
        }
        kit
    }
}

impl Default for Create {
    fn default() -> Self {
        Self {
            network: None,
            app_id: DEFAULT_APP_ID.to_string(),
        }
    }
}
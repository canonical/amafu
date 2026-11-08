pub mod animator;
pub mod seed;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use amafu_api::{ApiState, Principal, RouterTransport, TokenAuthenticator, TokenIdentity, Transport, router};
use amafu_dashboard::{DashboardState, SessionKey, dashboard_router};
use amafu_domain::ProjectName;
use amafu_domain::access::Role;
use amafu_interfaces::{Clock, RandomIdentifiers, Store};
use amafu_simulated_world::fakes::{MemoryAuditLog, MemoryStore};
use futures::future::BoxFuture;
use time::OffsetDateTime;

pub use animator::Animator;

pub const ADMIN_TOKEN: &str = "amafu-demo-admin";
pub const MEMBER_TOKEN: &str = "amafu-demo-member";
pub const VIEWER_TOKEN: &str = "amafu-demo-viewer";
const SESSION_LIFETIME: Duration = Duration::from_secs(8 * 3600);

struct WallClock;

impl Clock for WallClock {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }

    fn sleep(&self, duration: Duration) -> BoxFuture<'static, ()> {
        Box::pin(tokio::time::sleep(duration))
    }
}

fn identity(role: Role, token: &str) -> anyhow::Result<TokenIdentity> {
    Ok(TokenIdentity::for_token(
        Principal { name: format!("demo-{role}"), role, project: ProjectName::new(seed::DEFAULT_PROJECT)? },
        token,
        None,
    ))
}

pub struct DemoCluster {
    pub store: Arc<dyn Store>,
    pub dashboard: axum::Router,
}

impl DemoCluster {
    pub async fn start() -> anyhow::Result<Self> {
        let store: Arc<dyn Store> = Arc::new(MemoryStore::new());
        seed::seed(store.as_ref()).await?;
        let mut animator = Animator::new(Arc::clone(&store));
        animator.tick().await?;
        animator.tick().await?;
        tokio::spawn(animator.run());
        let api = router(ApiState {
            store: Arc::clone(&store),
            authenticator: Arc::new(TokenAuthenticator::new(vec![
                identity(Role::Admin, ADMIN_TOKEN)?,
                identity(Role::Member, MEMBER_TOKEN)?,
                identity(Role::Viewer, VIEWER_TOKEN)?,
            ])),
            audit: Arc::new(MemoryAuditLog::new()),
            clock: Arc::new(WallClock),
            identifiers: Arc::new(RandomIdentifiers),
        });
        let upstream: Arc<dyn Transport> = Arc::new(RouterTransport::new(api));
        let key = SessionKey::from_hexadecimal(&SessionKey::generate_hexadecimal()?)?;
        let dashboard = dashboard_router(DashboardState::new(key, vec![upstream], SESSION_LIFETIME));
        Ok(Self { store, dashboard })
    }

    pub async fn serve(self, listen: SocketAddr) -> anyhow::Result<()> {
        let listener = tokio::net::TcpListener::bind(listen).await?;
        Err(amafu_api::server::serve(listener, None, self.dashboard).await.into())
    }
}

//! A client session's lifetime (EXEC-DIST-9). The kernel frees a session's states and frames once its live streams
//! have all dropped and nothing of its own runs or waits: no request it is serving, no work a host reports pending.
//! Hosts report only I/O facts (a stream opened or dropped, work outstanding); the kernel decides.

use super::*;
use orbital_core_base::sync::Mutex;

/// Whether a host still has work in flight for a session (a keyed completion, a stream, a subscription of its own).
pub trait SessionPending: Send + Sync {
    fn pending(&self, session: &str) -> bool;
}

/// What an ended session leaves behind: the traits whose state it held, the only ones a reviving mount's carried copy
/// restores (EXEC-DIST-9).
pub(super) type EndedSession = Box<[Box<str>]>;

#[derive(Default)]
struct Life {
    streams: usize,
    dropped: bool,
    active: usize,
}

/// Every client session's lifetime, and what freeing one drops.
pub(super) struct SessionLives {
    lives: Mutex<HashMap<String, Life>>,
    pending: RwLock<Option<Arc<dyn SessionPending>>>,
    mounts: Arc<RwLock<mount_ledger::MountLedger>>,
    views: Arc<view_record::ViewRecords>,
    frames: Arc<RwLock<HashMap<(String, String), SharedEntity>>>,
    carried_rows: Arc<RwLock<HashMap<(String, String), String>>>,
    state_manager: Arc<RwLock<StateMachineManager>>,
    ended: Arc<RwLock<HashMap<String, EndedSession>>>,
}

impl SessionLives {
    pub(super) fn new(
        mounts: Arc<RwLock<mount_ledger::MountLedger>>,
        views: Arc<view_record::ViewRecords>,
        frames: Arc<RwLock<HashMap<(String, String), SharedEntity>>>,
        carried_rows: Arc<RwLock<HashMap<(String, String), String>>>,
        state_manager: Arc<RwLock<StateMachineManager>>,
        ended: Arc<RwLock<HashMap<String, EndedSession>>>,
    ) -> Self {
        Self { lives: Mutex::new(HashMap::new()), pending: RwLock::new(None), mounts, views, frames, carried_rows, state_manager, ended }
    }

    /// Free `session` and mark it ended: a request but a mount to it is refused until a mount revives it.
    pub(super) fn free(&self, session: &str) {
        let held = dispatch_scope::drop_session_instances(&self.frames, &self.carried_rows, &self.state_manager, session);
        self.views.drop_session(session);
        self.mounts.write().release(session);
        self.ended.write().insert(session.to_string(), held.into_iter().map(String::into_boxed_str).collect());
    }

    /// Free every session whose streams have all dropped and that is quiet. The freeing happens under the lives lock,
    /// so a request is admitted either before (it keeps the session) or after (it meets the ended session), never between.
    fn free_quiet(&self) {
        let pending = self.pending.read().clone();
        let mut lives = self.lives.lock();
        let quiet: Vec<String> = lives
            .iter()
            .filter(|(session, life)| life.dropped && life.active == 0 && !pending.as_ref().is_some_and(|p| p.pending(session)))
            .map(|(session, _)| session.clone())
            .collect();
        for session in quiet {
            lives.remove(&session);
            self.free(&session);
        }
    }

    /// Admit one request into `client`'s session, atomically with freeing: `Err` for an ended session unless the
    /// request is a mount, which revives it and answers the traits whose state the session held when it ended.
    fn admit(&self, client: &str, mounting: bool) -> Result<std::collections::BTreeSet<String>, SessionEnded> {
        let mut lives = self.lives.lock();
        let restored = if self.ended.read().contains_key(client) {
            if !mounting {
                return Err(SessionEnded(client.to_string()));
            }
            self.ended.write().remove(client).map(|held| held.iter().map(|name| name.to_string()).collect()).unwrap_or_default()
        } else {
            Default::default()
        };
        lives.entry(client.to_string()).or_default().active += 1;
        Ok(restored)
    }
}

/// One open live stream of a session; dropping it is the stream's end.
pub struct SessionStream {
    lives: Arc<SessionLives>,
    session: String,
}

impl Drop for SessionStream {
    fn drop(&mut self) {
        if let Some(life) = self.lives.lives.lock().get_mut(&self.session) {
            life.streams = life.streams.saturating_sub(1);
            life.dropped = life.streams == 0;
        }
        self.lives.free_quiet();
    }
}

/// One request a session is serving, held from its start to its end (a parked dispatch included), so the session is
/// not freed under it.
pub struct SessionActive {
    lives: Arc<SessionLives>,
    session: String,
}

impl SessionActive {
    pub fn session(&self) -> &str {
        &self.session
    }
}

impl Drop for SessionActive {
    fn drop(&mut self) {
        if let Some(life) = self.lives.lives.lock().get_mut(&self.session) {
            life.active = life.active.saturating_sub(1);
        }
        self.lives.free_quiet();
    }
}

/// A request's session as the kernel admits it (EXEC-DIST-9).
pub enum RequestSession {
    /// The program session: the request names no client.
    Program,
    /// A scratch session of its own, dropped when the guard drops (a client request naming no client).
    Scratch(SessionGuard),
    /// The client's session, held active while the request runs. `restored`: when the request is a mount reviving an
    /// ended session, the traits whose state that session held; the mount's carried copy restores only those.
    Client { active: SessionActive, restored: std::collections::BTreeSet<String> },
}

impl RuntimeKernel {
    /// The host's report of work in flight per session, read before a quiet session is freed.
    pub fn set_session_pending(&self, pending: Arc<dyn SessionPending>) {
        *self.session_lives.pending.write() = Some(pending);
    }

    /// `session` opened a live stream; the returned hold is the stream, and dropping it is its end.
    pub fn open_session_stream(&self, session: &str) -> SessionStream {
        let mut lives = self.session_lives.lives.lock();
        let life = lives.entry(session.to_string()).or_default();
        life.streams += 1;
        life.dropped = false;
        SessionStream { lives: Arc::clone(&self.session_lives), session: session.to_string() }
    }

    /// Admit a request into its session (EXEC-DIST-9): a request naming `client` runs in that client's session, held
    /// active until the returned hold drops; to an ended session only a mount is admitted, and it revives the session.
    /// A request from a client (`wire`) naming none gets a scratch session; any other runs in the program session.
    pub fn admit_request(&self, client: Option<&str>, mounting: bool, wire: bool) -> Result<RequestSession, SessionEnded> {
        let Some(client) = client else {
            return Ok(if wire { RequestSession::Scratch(self.enter_scratch_session()) } else { RequestSession::Program });
        };
        let restored = self.session_lives.admit(client, mounting)?;
        Ok(RequestSession::Client {
            active: SessionActive { lives: Arc::clone(&self.session_lives), session: client.to_string() },
            restored,
        })
    }

    /// Free every quiet session whose streams have dropped: a host calls it when work it reported pending ends.
    pub fn free_quiet_sessions(&self) {
        self.session_lives.free_quiet();
    }
}

/// A request but a mount to an ended session (EXEC-DIST-9, `SESSION_ENDED`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEnded(pub String);

impl std::fmt::Display for SessionEnded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "session {} ended: mount again with the frames the client holds", self.0)
    }
}

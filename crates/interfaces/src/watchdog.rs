pub trait Watchdog: Send + Sync {
    fn feed(&self) -> std::io::Result<()>;
}

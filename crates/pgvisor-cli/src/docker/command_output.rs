/// Captured output from executing an external command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    /// Process exit status code (or `-1` if terminated by signal).
    pub status_code: i32,
    /// Captured standard output.
    pub stdout: String,
    /// Captured standard error.
    pub stderr: String,
}

impl CommandOutput {
    /// Returns `true` when the command exited with status `0`.
    pub fn success(&self) -> bool {
        self.status_code == 0
    }
}

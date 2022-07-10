use std::error::Error;

#[derive(Debug)]
pub enum ErrorSeverity {
    Internal,
    UserInput
}
#[derive(Debug)]
pub enum ErrorKind {
    NotInVoiceChannel,
    InternalError,
    VoiceNotEnabled,
    MustProvideURL,
    MusicDataNotInitialized,
    MessageNotInGuildChannel,
    NoSongCurrentlyPlaying
}

pub type GuildMusicResult<T> = Result<T, DiscordCommandError>;

#[derive(Debug)]
pub struct DiscordCommandError {
    pub kind: ErrorKind,
    pub severity: ErrorSeverity,
    pub source: Option<Box<dyn Error + Send + Sync>>
}

impl std::fmt::Display for DiscordCommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "severity: {:?}, kind: {:?}", self.severity, self.kind)
    }
}

impl std::error::Error for DiscordCommandError {}

pub mod music;
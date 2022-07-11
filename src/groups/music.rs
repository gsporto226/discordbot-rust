use super::{DiscordCommandError, GuildMusicResult};
use crate::{utils::ArcMut, SONGBIRD};
use log::debug;
use parking_lot::Mutex;
use serenity::{
    async_trait,
    client::Context,
    framework::standard::{
        macros::{command, group},
        Args, CommandResult
    },
    model::{
        channel::Message,
        id::{ChannelId, GuildId, UserId}, guild,
    },
    prelude::{Mutex as AsyncMutex, TypeMapKey},
};
use songbird::{
    create_player,
    input::Restartable,
    tracks::{Track, TrackHandle},
    Call, Event, EventContext, EventHandler as VoiceEventHandler,
    TrackEvent,
};
use uuid::Uuid;
use std::{collections::HashMap, sync::Arc, time::Instant};

struct GuildMusicHashMapKey;
type TrackPair = (Track, TrackHandle);
type GuildMusicHashmap = HashMap<GuildId, Arc<GuildMusic>>;

impl TypeMapKey for GuildMusicHashMapKey {
    type Value = ArcMut<GuildMusicHashmap>;
}

lazy_static! {
    static ref GUILD_MUSIC_HASHMAP: ArcMut<GuildMusicHashmap> =
        Arc::new(Mutex::new(HashMap::new()));
}

#[derive(Debug)]
struct MusicRequest {
    uuid: uuid::Uuid,
    url: String,
    channel_id: ChannelId,
    _requested_by: UserId
}

#[derive(Debug)]
struct LoadedMusic {
    uuid: Uuid,
    track_pair: Option<TrackPair>
}

#[derive(Debug)]
struct NowPlaying {
    uuid: Uuid,
    track_handle: TrackHandle
}

#[derive(Debug)]
pub struct GuildMusicData {
    guild_id: GuildId,
    queue: Vec<MusicRequest>,
    preloaded: Option<LoadedMusic>,
    now_playing: Option<NowPlaying>,
    call_pair: Option<(ChannelId, Arc<AsyncMutex<Call>>)>,
}

#[allow(clippy::module_name_repetitions)]
pub struct GuildMusic {
    pub guild_music_data: Arc<Mutex<GuildMusicData>>,
}

async fn get_ytdl_track(url: String) -> Option<TrackPair> {
    match Restartable::ytdl(url, false).await {
        Ok(restartable) => Some(create_player(restartable.into())),
        Err(_error) => None,
    }
}

async fn get_call(guild_id: GuildId, channel_id: ChannelId) -> Option<Arc<AsyncMutex<Call>>> {
    let (call_lock, result) = SONGBIRD.join(guild_id, channel_id).await;
    if let Ok(_channel) = result {
        Some(call_lock)
    } else {
        None
    }
}

impl GuildMusic {
    pub fn new(guild_id: GuildId) -> Self {
        Self {
            guild_music_data: Arc::new(Mutex::new(GuildMusicData::new(guild_id))),
        }
    }

    fn tick_guild_music(&self) {
        let mut guild_music_data = self.guild_music_data.lock();
        if let Some((guild_id, next, track_pair_option, current_call_option)) = {
            if guild_music_data.now_playing.is_none() {
                if let Some(next) = guild_music_data.queue.pop() {
                    debug!("Next is {:?}", next);
                    let track_pair = guild_music_data.preloaded.take().and_then(|preloaded| {
                        preloaded.track_pair.and_then(|track_pair| {
                            if preloaded.uuid == next.uuid {
                                Some(track_pair)
                            } else {
                                None
                            }
                        })
                    });
                    let current_call = guild_music_data.call_pair.as_ref().and_then(|(channel_id, current_call_lock)| {
                        if channel_id.0 == next.channel_id.0 {
                            Some(current_call_lock.clone())
                        } else {
                            None
                        }
                    });
                    Some((guild_music_data.guild_id, next, track_pair, current_call))
                } else {
                    None
                }
            } else {
                None
            }
        } {
            let play_guild_music_data_lock = self.guild_music_data.clone();
            tokio::spawn(async move {
                match {
                    if track_pair_option.is_some() {
                        track_pair_option
                    } else {
                        get_ytdl_track(next.url).await
                    }
                } {
                    Some((track, track_handle)) => {
                        match {
                            if current_call_option.is_some() {
                                current_call_option
                            } else {
                                get_call(guild_id, next.channel_id).await
                            }
                        } {
                            Some(call_lock) => {
                                {
                                    play_guild_music_data_lock.lock().now_playing = Some(NowPlaying { uuid: next.uuid, track_handle });
                                }
                                let elapsed = Instant::now();
                                let mut call = call_lock.lock().await;
                                debug!("Spent {}ms waiting for call lock", elapsed.elapsed().as_millis());
                                call.play(track);
                                call.remove_all_global_events();
                                call.add_global_event(
                                    Event::Track(TrackEvent::End),
                                    TrackEventListener { guild_id, music_uuid: next.uuid }
                                );
                            }
                            None => {
                                // reply with failure to get voice channel
                            }
                        };
                    }
                    None => {
                        // reply with failure
                    }
                }
            });
        };
        let music_to_preload = guild_music_data.queue.get(0).map(|music_request| {
            (music_request.uuid, music_request.url.clone())
        });
        if let Some((uuid, _url)) = &music_to_preload {
            guild_music_data.preloaded = Some(LoadedMusic { uuid: *uuid, track_pair: None });
        };
        if let Some((uuid, url)) = music_to_preload {
            let preload_guild_music_data_lock = self.guild_music_data.clone();
            tokio::spawn(async move {
                debug!("Preloading for uuid {} and url {}", uuid, url);
                let time_elapsed = Instant::now();
                if let Some(track_pair) = get_ytdl_track(url).await {
                    debug!("Finished preloading for uuid {} in {}ms", uuid, time_elapsed.elapsed().as_millis());
                    if let Some(ref mut preloaded) = preload_guild_music_data_lock.lock().preloaded {
                        if preloaded.uuid == uuid {
                            preloaded.track_pair = Some(track_pair);
                        };
                    };
                };
            });
        };
    }

    pub fn insert_music(&self, url: String, channel_id: ChannelId, requested_by: UserId) {
        {
            let mut guild_music_data = self.guild_music_data.lock();
            guild_music_data.queue.push(MusicRequest {
                uuid: Uuid::new_v4(),
                url,
                channel_id,
                _requested_by: requested_by
            });
            debug!("Inserted music into guild {:?}", guild_music_data.guild_id);
        }
        self.tick_guild_music();
    }

    pub fn handle_track_event(&self, _ctx: &EventContext<'_>, music_uuid: Uuid) {
        {
            let mut guild_music_data = self.guild_music_data.lock();
            if let Some(now_playing) = &guild_music_data.now_playing {
                if now_playing.uuid == music_uuid {
                    if now_playing.track_handle.stop().is_err() {};
                    guild_music_data.now_playing = None;
                };
            }
        }
        self.tick_guild_music();
    }

    pub fn skip(&self) -> GuildMusicResult<()> {
        let guild_music_data = self.guild_music_data.lock();
        if let Some(now_playing) = &guild_music_data.now_playing {
            if let Err(why) = now_playing.track_handle.stop() {
                Err( DiscordCommandError {
                    kind: super::ErrorKind::InternalError,
                    source: Some(Box::new(why)),
                    severity: super::ErrorSeverity::Internal
                })
            } else {
                Ok(())
            }
        } else {
            Err( DiscordCommandError { 
                kind: super::ErrorKind::NoSongCurrentlyPlaying,
                source: None,
                severity: super::ErrorSeverity::UserInput
            })
        }
    }
}

impl GuildMusicData {
    pub(crate) fn new(guild_id: GuildId) -> Self {
        Self {
            guild_id,
            queue: vec![],
            preloaded: None,
            now_playing: None,
            call_pair: None,
        }
    }
}

#[group]
#[description = "music_group_description"]
#[only_in("guilds")]
#[commands(play, skip)]
pub struct Music;

fn queue_is_not_empty(guild_music_data: GuildMusicData) -> bool {
    todo!();
}

struct TrackEventListener {
    guild_id: GuildId,
    music_uuid: Uuid
}

#[async_trait]
impl VoiceEventHandler for TrackEventListener {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        if let Some(guild_music) = GUILD_MUSIC_HASHMAP.lock().get(&self.guild_id) {
            guild_music.handle_track_event(ctx, self.music_uuid);
        };
        None
    }
}

async fn get_guild_music_for(context: &Context, guild_id: GuildId) -> Arc<GuildMusic> {
    let mut data = context.data.write().await;
    let mut guild_music_hashmap = GUILD_MUSIC_HASHMAP.lock();
    let guild_music = guild_music_hashmap
        .entry(guild_id)
        .or_insert_with(|| Arc::new(GuildMusic::new(guild_id)));
    if !data.contains_key::<GuildMusicHashMapKey>() {
        data.insert::<GuildMusicHashMapKey>(GUILD_MUSIC_HASHMAP.clone());
    };
    guild_music.clone()
}

async fn get_voice_guild_and_channel(
    context: &Context,
    message: &Message,
) -> Option<(GuildId, ChannelId)> {
    if let Some(guild) = message.guild(&context.cache).await {
        let guild_id = guild.id;
        let channel_id = guild
            .voice_states
            .get(&message.author.id)
            .and_then(|voice_state| voice_state.channel_id);
        if let Some(channel_id) = channel_id {
            return Some((guild_id, channel_id));
        };
    }
    None
}

#[command]
#[aliases("p", "ply")]
async fn play(context: &Context, message: &Message, mut args: Args) -> CommandResult {
    let requested_by = message.author.id;
    match get_voice_guild_and_channel(context, message).await {
        Some((guild_id, channel_id)) => {
            if let Ok(url) = args.single::<String>() {
                get_guild_music_for(context, guild_id).await.insert_music(
                    url,
                    channel_id,
                    requested_by,
                );
                Ok(())
            } else {
                Err(DiscordCommandError {
                    source: None,
                    severity: super::ErrorSeverity::UserInput,
                    kind: super::ErrorKind::MustProvideURL,
                }
                .into())
            }
        }
        None => Err(DiscordCommandError {
            source: None,
            severity: super::ErrorSeverity::UserInput,
            kind: super::ErrorKind::NotInVoiceChannel,
        }
        .into()),
    }
}

// async fn join(manager: Arc<Songbird>, guild_id: GuildId, channel_id: ChannelId) -> Result<Arc<Mutex<Call>>, DiscordCommandError> {
//     let (call, result) = manager.join(guild_id, channel_id).await;
//     if let Err(error) = result {
//         return Err(DiscordCommandError { kind: super::ErrorKind::InternalError, severity: super::ErrorSeverity::Internal, source: Some(Box::new(error)) });
//     };
//     Ok(call)
// }
// #[command]
// async fn stop() {}
#[command]
#[aliases("s", "skp")]
async fn skip(context: &Context, message: &Message, mut _args: Args) -> CommandResult {
    if let Some(guild_id) = message.guild_id {
        if let Err(err) = get_guild_music_for(context, guild_id).await.skip() {
            Err(err.into())
        } else {
            Ok(())
        }
    } else {
        Err(DiscordCommandError {
            source: None,
            severity: super::ErrorSeverity::UserInput,
            kind: super::ErrorKind::MessageNotInGuildChannel,
        }
        .into())
    }
}
// #[command]
// async fn queue() {}
// async fn scramble() {}
// async fn remove() {}
// async fn history() {}
// async fn favorites() {}

//! The ron Asset Loader.

use bevy::{
    asset::{io::Reader, AssetLoader, AsyncReadExt, LoadContext},
    log::error,
    utils::BoxedFuture,
};
use indexmap::IndexMap;
use serde_ron::de::from_bytes;
use thiserror::Error;

use crate::prelude::{Action, ActionId, Actor, ActorId, TalkData};

use super::types::RonTalk;

/// Load Talks from json assets.
pub struct TalksLoader;

/// The error type for the RON Talks loader.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum RonLoaderError {
    /// An [IO Error](std::io::Error)
    #[error("Could not read the file: {0}")]
    Io(#[from] std::io::Error),
    /// A [RON Error](ron::error::SpannedError)
    #[error("Could not parse RON: {0}")]
    RonError(#[from] serde_ron::error::SpannedError),
    /// Multiple actions have same id error
    #[error("multiple actions have same id: {0}")]
    DuplicateActionId(ActionId),
    /// The actor id is duplicated
    #[error("the actor id {0} is duplicated")]
    DuplicateActorId(String),
}

impl AssetLoader for TalksLoader {
    type Asset = TalkData;
    type Settings = ();
    type Error = RonLoaderError;

    fn load<'a>(
        &'a self,
        reader: &'a mut Reader,
        _settings: &'a Self::Settings,
        _load_context: &'a mut LoadContext,
    ) -> BoxedFuture<'a, Result<Self::Asset, Self::Error>> {
        Box::pin(async move {
            let mut bytes = Vec::new();
            reader.read_to_end(&mut bytes).await?;
            let ron_talk = from_bytes::<RonTalk>(&bytes)?;

            // build a RawTalk Asset from the RonTalk

            // 1. Build the actors vec
            let actors = ron_talk.actors;
            let mut talk_actors = IndexMap::<ActorId, Actor>::with_capacity(actors.len());
            // let mut asset_deps = vec![];
            for actor in actors {
                let talk_actor = Actor { name: actor.name };
                let id = actor.id;
                if talk_actors.insert(id.clone(), talk_actor).is_some() {
                    return Err(RonLoaderError::DuplicateActorId(id));
                }
            }

            // 2. build the raw_actions vec
            let mut raw_actions =
                IndexMap::<ActionId, Action>::with_capacity(ron_talk.script.len());
            for action in ron_talk.script {
                let id = action.id;
                if raw_actions.insert(id, action.into()).is_some() {
                    return Err(RonLoaderError::DuplicateActionId(id));
                }
            }

            let raw_talk = TalkData {
                actors: talk_actors,
                script: raw_actions,
            };

            Ok(raw_talk)
        })
    }

    fn extensions(&self) -> &[&str] {
        &["talk.ron"]
    }
}

#[cfg(test)]
mod tests {
    use bevy::prelude::{AssetServer, Assets, Handle};

    use crate::{prelude::TalkData, tests::minimal_app};

    #[test]
    fn test_parse_raw_talk() {
        let mut app = minimal_app();
        let asset_server = app.world.get_resource::<AssetServer>();
        assert!(asset_server.is_some());

        let asset_server = asset_server.unwrap();
        let talk_handle: Handle<TalkData> = asset_server.load("talks/simple.talk.ron");
        app.update();
        app.update();

        let talk_assets = app.world.get_resource::<Assets<TalkData>>();
        assert!(talk_assets.is_some());

        let talk_assets = talk_assets.unwrap();
        let talk = talk_assets.get(&talk_handle);
        assert!(talk.is_some());

        let talk = talk.unwrap();
        assert_eq!(talk.actors.len(), 2);
        assert_eq!(talk.script.len(), 13);
    }
}

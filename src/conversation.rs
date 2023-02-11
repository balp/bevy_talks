use bevy::{prelude::default, reflect::TypeUuid, utils::HashMap};
use petgraph::{prelude::DiGraph, stable_graph::NodeIndex, visit::EdgeRef};

use crate::{
    errors::{ConversationError, ScriptParsingError},
    script::{ActionId, Actor, ActorAction, ActorOrPlayerActionJSON, Choice, RawScript},
};

#[derive(Debug, TypeUuid)]
#[uuid = "413be529-bfeb-8c5b-9db0-4b8b380a2c47"]
pub struct Conversation {
    graph: DiGraph<ConvoNode, ()>,
    current: NodeIndex,
    id_to_nodeidx: HashMap<ActionId, NodeIndex>,
}

impl Conversation {
    pub(crate) fn new(raw_script: RawScript) -> Result<Self, ScriptParsingError> {
        if raw_script.script.is_empty() {
            return Err(ScriptParsingError::EmptyScript);
        }
        let mut graph: DiGraph<ConvoNode, ()> = DiGraph::new();

        let mut start_action = Option::<NodeIndex>::None;

        // 1. Build auxiliary maps

        // ActionId => next_id map, so we can fill the next when it's None
        // (it means point to the next action) and throw duplicate id error
        let id_to_next_map = build_id_to_next_map(&raw_script.script)?;

        // ActionId => (NodeIndex, next_id) map so we can keep track of what we added in the graph.
        // Right now ActionId == NodeIndex so not really needed, but I'd like to have uuids as ids in the future
        let mut id_to_nodeids_map: HashMap<ActionId, StrippedNodeAction> =
            HashMap::with_capacity(raw_script.script.len());

        // 2. Add all actions as nodes with some validation
        for action in raw_script.script {
            let this_action_id = action.id();
            let start_flag = action.start();

            // Grab the nexts in the choices for later validation
            let choices_nexts = action
                .choices()
                .map(|vc| vc.iter().map(|c| c.next).collect());

            // 2.a add the node to the graph
            let node_idx = add_action_node(&mut graph, action, &raw_script.actors)?;

            // 2.b check if this is the starting action
            if check_start_flag(start_flag, start_action.is_some())? {
                start_action = Some(node_idx);
            }

            // 2.b add info (idx, next_id) as we build the graph
            if id_to_nodeids_map
                .insert(
                    this_action_id,
                    StrippedNodeAction {
                        node_idx,
                        next_action_id: id_to_next_map.get(&this_action_id).copied(),
                        choices: choices_nexts,
                    },
                )
                .is_some()
            {
                return Err(ScriptParsingError::RepeatedId(this_action_id));
            };
        }

        // 3 Validate all the nexts (they should point to existing actions)
        validate_nexts(&id_to_nodeids_map)?;

        // 4 Add edges to the graph
        for (action_id, node_action) in &id_to_nodeids_map {
            // 5.a With the next field, add a single edge
            if let Some(next_id) = node_action.next_action_id {
                let next_node_action = id_to_nodeids_map
                    .get(&next_id)
                    .ok_or(ScriptParsingError::NextActionNotFound(*action_id, next_id))?;

                graph.add_edge(node_action.node_idx, next_node_action.node_idx, ());
            }

            // 5.b With the choices, add an edge for each choice
            if let Some(choices) = &node_action.choices {
                for choice in choices {
                    let next_node_action = id_to_nodeids_map
                        .get(&choice)
                        .ok_or(ScriptParsingError::NextActionNotFound(*action_id, *choice))?;

                    graph.add_edge(node_action.node_idx, next_node_action.node_idx, ());
                }
            }
        }

        // 5. We can drop the next/choices now and just keep action_id => NodeIndex
        let id_to_nodeidx = id_to_nodeids_map
            .into_iter()
            .map(|(id, node_act)| (id, node_act.node_idx))
            .collect();

        Ok(Self {
            graph,
            current: start_action.ok_or(ScriptParsingError::NoStartingAction)?,
            id_to_nodeidx,
        })
    }

    // pub fn current_text(&self) -> &str {
    //     &self.dialogue_graph[self.current].text
    // }

    pub fn next_line(&mut self) -> Result<(), ConversationError> {
        let dnode = self.graph.node_weight(self.current);

        // if for some reason the current node is not in the graph, return an error
        let cur_dial = dnode.ok_or(ConversationError::InvalidDialogue)?;

        // if the current dialogue has choices, return an error
        if cur_dial.choices.is_some() {
            return Err(ConversationError::ChoicesNotHandled);
        }

        let edge_ref = self
            .graph
            .edges(self.current)
            .next()
            .ok_or(ConversationError::NoNextDialogue)?;

        // TODO: wait, what is this NodeId? Is it the NodeIndex? I'm not sure
        self.current = edge_ref.target();
        Ok(())
    }

    pub fn jump_to(&mut self, id: i32) -> Result<(), ConversationError> {
        let idx = self
            .id_to_nodeidx
            .get(&id)
            .ok_or(ConversationError::WrongJump(id))?;

        self.current = *idx;
        Ok(())
    }

    /// Returns the choices for the current dialogue. If there are no choices, returns an error.
    pub fn choices(&self) -> Result<Vec<Choice>, ConversationError> {
        let dnode = self.graph.node_weight(self.current);
        // if for some reason the current node is not in the graph, return an error
        let cur_dial = dnode.ok_or(ConversationError::InvalidDialogue)?;

        if let Some(choices) = &cur_dial.choices {
            Ok(choices.clone())
        } else {
            Err(ConversationError::NoChoices)
        }
    }

    // pub fn current_talker(&self) -> Option<Actor> {
    //     let dnode = self.dialogue_graph.node_weight(self.current)?;
    //     dnode.actor.clone()
    // }
}
#[derive(Debug, Default)]
struct ConvoNode {
    text: Option<String>,
    actors: Option<Vec<Actor>>,
    choices: Option<Vec<Choice>>,
}

/// A minimal representation of a convo node for validation purposes
#[derive(Debug)]
struct StrippedNodeAction {
    node_idx: NodeIndex,
    next_action_id: Option<ActionId>,
    choices: Option<Vec<ActionId>>,
}

fn build_id_to_next_map(
    script: &Vec<ActorOrPlayerActionJSON>,
) -> Result<HashMap<ActionId, ActionId>, ScriptParsingError> {
    let mut id_to_next_map: HashMap<ActionId, ActionId> = HashMap::with_capacity(script.len() - 1);
    for (i, a) in script.iter().enumerate() {
        match a.next() {
            Some(n) => {
                if id_to_next_map.insert(a.id(), n).is_some() {
                    return Err(ScriptParsingError::RepeatedId(a.id()));
                }
            }
            None => {
                // if next not defined:
                // either player action (with choices) or actor action pointing to the one below it
                // NOTE: we are not adding the last action (if next: None) as it can't have a next
                if i + 1 < script.len() {
                    id_to_next_map.insert(a.id(), script[i + 1].id());
                }
            }
        };
    }
    Ok(id_to_next_map)
}

fn extract_actors(
    aaction: &ActorAction,
    actors_map: &HashMap<String, Actor>,
) -> Result<Option<Vec<Actor>>, ScriptParsingError> {
    // TODO: this is a bit verbose and I bet there is some functional magic to do this better

    // if actors is None, keep it None.
    // Otherwise, retrieve them from the actors map. In case one is not found, return an error.
    match &aaction.actors {
        Some(actors_vec) => {
            // For the great majority of times, there will be only one actor
            let mut actors = Vec::with_capacity(1);
            for a in actors_vec {
                actors.push(
                    actors_map
                        .get(a)
                        .ok_or(ScriptParsingError::ActorNotFound(aaction.id, a.to_string()))?
                        .to_owned(),
                );
            }
            Ok(Some(actors))
        }
        None => Ok(None),
    }
}

fn check_start_flag(
    start_flag: Option<bool>,
    already_have_start: bool,
) -> Result<bool, ScriptParsingError> {
    if let Some(true) = start_flag {
        if already_have_start {
            return Err(ScriptParsingError::MultipleStartingAction);
        }
        return Ok(true);
    }
    Ok(false)
}

fn add_action_node(
    graph: &mut DiGraph<ConvoNode, ()>,
    action: ActorOrPlayerActionJSON,
    actors_map: &HashMap<String, Actor>,
) -> Result<NodeIndex, ScriptParsingError> {
    let mut node = ConvoNode { ..default() };
    match action {
        ActorOrPlayerActionJSON::Actor(actor_action) => {
            node.actors = extract_actors(&actor_action, actors_map)?;
            node.text = actor_action.text;
        }
        ActorOrPlayerActionJSON::Player(player_action) => {
            node.choices = Some(player_action.choices);
        }
    }
    let node_idx = graph.add_node(node);
    Ok(node_idx)
}

fn validate_nexts(
    nodeidx_dialogue_map: &HashMap<i32, StrippedNodeAction>,
) -> Result<(), ScriptParsingError> {
    for (id, stripped_node) in nodeidx_dialogue_map {
        if let Some(next_id) = stripped_node.next_action_id {
            if !nodeidx_dialogue_map.contains_key(&next_id) {
                return Err(ScriptParsingError::NextActionNotFound(*id, next_id));
            }
        } else if let Some(vc) = &stripped_node.choices {
            for c in vc {
                if !nodeidx_dialogue_map.contains_key(c) {
                    return Err(ScriptParsingError::NextActionNotFound(*id, *c));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::script::{ActorAction, ActorOrPlayerActionJSON, PlayerAction};
    use bevy::prelude::default;

    fn an_actors_map(name: String) -> HashMap<String, Actor> {
        let mut actors = HashMap::new();
        actors.insert(
            name,
            Actor {
                name: "Bob".to_string(),
                asset: "bob.png".to_string(),
            },
        );
        actors
    }

    // 'new' tests
    #[test]
    fn no_script_err() {
        let raw_script = RawScript {
            actors: default(),
            script: default(),
        };

        let convo = Conversation::new(raw_script).err();
        assert_eq!(convo, Some(ScriptParsingError::EmptyScript));
    }

    #[test]
    fn actor_not_found_err() {
        let raw_script = RawScript {
            actors: default(),
            script: vec![ActorOrPlayerActionJSON::Actor(ActorAction {
                text: Some("Hello".to_string()),
                actors: Some(vec!["Bob".to_string()]),
                start: Some(true),
                ..default()
            })],
        };

        let convo = Conversation::new(raw_script).err();
        assert_eq!(
            convo,
            Some(ScriptParsingError::ActorNotFound(0, "Bob".to_string()))
        );
    }

    #[test]
    fn actor_not_found_with_mismath_err() {
        let raw_talk = RawScript {
            actors: an_actors_map("Bob".to_string()),
            script: vec![ActorOrPlayerActionJSON::Actor(ActorAction {
                actors: Some(vec!["Alice".to_string()]),
                start: Some(true),
                ..default()
            })],
        };

        let convo = Conversation::new(raw_talk).err();
        assert_eq!(
            convo,
            Some(ScriptParsingError::ActorNotFound(0, "Alice".to_string()))
        );
    }

    #[test]
    fn no_start_err() {
        let raw_talk = RawScript {
            actors: an_actors_map("Alice".to_string()),
            script: vec![ActorOrPlayerActionJSON::Actor(ActorAction {
                actors: Some(vec!["Alice".to_string()]),

                ..default()
            })],
        };

        let convo = Conversation::new(raw_talk).err();
        assert_eq!(convo, Some(ScriptParsingError::NoStartingAction));
    }

    #[test]
    fn multiple_start_actor_action_err() {
        let raw_talk = RawScript {
            actors: default(),
            script: vec![
                ActorOrPlayerActionJSON::Actor(ActorAction {
                    start: Some(true),
                    ..default()
                }),
                ActorOrPlayerActionJSON::Actor(ActorAction {
                    start: Some(true),
                    ..default()
                }),
            ],
        };

        let convo = Conversation::new(raw_talk).err();
        assert_eq!(convo, Some(ScriptParsingError::MultipleStartingAction));
    }

    #[test]
    fn multiple_start_mixed_err() {
        let raw_talk = RawScript {
            actors: default(),
            script: vec![
                ActorOrPlayerActionJSON::Actor(ActorAction {
                    start: Some(true),
                    ..default()
                }),
                ActorOrPlayerActionJSON::Player(PlayerAction {
                    id: 3,
                    start: Some(true),
                    ..default()
                }),
            ],
        };

        let convo = Conversation::new(raw_talk).err();
        assert_eq!(convo, Some(ScriptParsingError::MultipleStartingAction));
    }

    #[test]
    fn multiple_start_player_action_err() {
        let raw_talk = RawScript {
            actors: default(),
            script: vec![
                ActorOrPlayerActionJSON::Player(PlayerAction {
                    start: Some(true),
                    ..default()
                }),
                ActorOrPlayerActionJSON::Player(PlayerAction {
                    start: Some(true),
                    ..default()
                }),
            ],
        };

        let convo = Conversation::new(raw_talk).err();
        assert_eq!(convo, Some(ScriptParsingError::MultipleStartingAction));
    }

    #[test]
    fn repeated_id_actor_err() {
        let raw_talk = RawScript {
            actors: default(),
            script: vec![
                ActorOrPlayerActionJSON::Actor(ActorAction {
                    id: 1,
                    text: Some("Hello".to_string()),
                    next: Some(1),
                    start: Some(true),
                    ..default()
                }),
                ActorOrPlayerActionJSON::Actor(ActorAction {
                    id: 1,
                    text: Some("Whatup".to_string()),
                    next: Some(2),
                    ..default()
                }),
            ],
        };

        let convo = Conversation::new(raw_talk).err();
        assert_eq!(convo, Some(ScriptParsingError::RepeatedId(1)));
    }

    #[test]
    fn repeated_id_mixed_err() {
        let raw_talk = RawScript {
            actors: default(),
            script: vec![
                ActorOrPlayerActionJSON::Actor(ActorAction {
                    id: 1,
                    text: Some("Hello".to_string()),
                    next: Some(1),
                    start: Some(true),
                    ..default()
                }),
                ActorOrPlayerActionJSON::Player(PlayerAction { id: 1, ..default() }),
            ],
        };

        let convo = Conversation::new(raw_talk).err();
        assert_eq!(convo, Some(ScriptParsingError::RepeatedId(1)));
    }

    #[test]
    fn repeated_id_player_err() {
        let raw_talk = RawScript {
            actors: default(),
            script: vec![
                ActorOrPlayerActionJSON::Player(PlayerAction {
                    id: 1,
                    start: Some(true),
                    ..default()
                }),
                ActorOrPlayerActionJSON::Player(PlayerAction { id: 1, ..default() }),
            ],
        };

        let convo = Conversation::new(raw_talk).err();
        assert_eq!(convo, Some(ScriptParsingError::RepeatedId(1)));
    }

    #[test]
    fn next_actor_action_not_found_err() {
        let raw_talk = RawScript {
            actors: default(),
            script: vec![ActorOrPlayerActionJSON::Actor(ActorAction {
                next: Some(2),
                start: Some(true),
                ..default()
            })],
        };

        let convo = Conversation::new(raw_talk).err();
        assert_eq!(convo, Some(ScriptParsingError::NextActionNotFound(0, 2)));
    }

    #[test]
    fn next_not_found_in_choice_err() {
        let raw_talk = RawScript {
            actors: default(),
            script: vec![ActorOrPlayerActionJSON::Player(PlayerAction {
                choices: vec![Choice {
                    text: "Whatup".to_string(),
                    next: 2,
                }],
                start: Some(true),
                ..default()
            })],
        };

        let convo = Conversation::new(raw_talk).err();
        assert_eq!(convo, Some(ScriptParsingError::NextActionNotFound(0, 2)));
    }

    #[test]
    fn new_with_one_action() {
        let raw_talk = RawScript {
            actors: default(),
            script: vec![ActorOrPlayerActionJSON::Actor(ActorAction {
                start: Some(true),
                ..default() // end: None,
            })],
        };

        let convo = Conversation::new(raw_talk).unwrap();
        assert_eq!(convo.graph.node_count(), 1);
        assert_eq!(convo.graph.edge_count(), 0);
        assert_eq!(convo.current, NodeIndex::new(0));
    }

    #[test]
    fn new_with_two_actor_action_nodes() {
        let raw_talk = RawScript {
            actors: default(),
            script: vec![
                ActorOrPlayerActionJSON::Actor(ActorAction {
                    id: 1,
                    next: Some(2),
                    start: Some(true),
                    ..default()
                }),
                ActorOrPlayerActionJSON::Actor(ActorAction { id: 2, ..default() }),
            ],
        };

        let convo = Conversation::new(raw_talk).unwrap();
        assert_eq!(convo.graph.node_count(), 2);
        assert_eq!(convo.graph.edge_count(), 1);
    }

    #[test]
    fn new_with_self_loop() {
        let raw_talk = RawScript {
            actors: default(),
            script: vec![ActorOrPlayerActionJSON::Actor(ActorAction {
                id: 1,
                next: Some(1),
                start: Some(true),
                ..default()
            })],
        };

        let convo = Conversation::new(raw_talk).unwrap();
        assert_eq!(convo.graph.node_count(), 1);
        assert_eq!(convo.graph.edge_count(), 1);
    }

    #[test]
    fn new_with_branching() {
        let raw_talk = RawScript {
            actors: default(),
            script: vec![
                ActorOrPlayerActionJSON::Player(PlayerAction {
                    choices: vec![
                        Choice {
                            text: "Choice 1".to_string(),
                            next: 2,
                        },
                        Choice {
                            text: "Choice 2".to_string(),
                            next: 3,
                        },
                    ],
                    start: Some(true),
                    ..default()
                }),
                ActorOrPlayerActionJSON::Actor(ActorAction {
                    id: 2,
                    text: Some("Hello".to_string()),
                    ..default()
                }),
                ActorOrPlayerActionJSON::Actor(ActorAction { id: 3, ..default() }),
            ],
        };

        let convo = Conversation::new(raw_talk).unwrap();
        assert_eq!(convo.graph.node_count(), 3);
        assert_eq!(convo.graph.edge_count(), 4);
        assert_eq!(convo.current, NodeIndex::new(0));
    }

    #[test]
    fn new_with_actors() {
        let mut actors_map: HashMap<String, Actor> = HashMap::new();
        actors_map.insert(
            "bob".to_string(),
            Actor {
                asset: "bob.png".to_string(),
                name: "Bob".to_string(),
            },
        );
        actors_map.insert(
            "alice".to_string(),
            Actor {
                name: "Alice".to_string(),
                asset: "alice.png".to_string(),
            },
        );

        let raw_talk = RawScript {
            actors: actors_map,
            script: vec![
                ActorOrPlayerActionJSON::Actor(ActorAction {
                    id: 1,
                    text: Some("Hello".to_string()),
                    actors: Some(vec!["bob".to_string()]),
                    next: Some(2),
                    start: Some(true),
                    ..default()
                }),
                ActorOrPlayerActionJSON::Actor(ActorAction {
                    id: 2,
                    text: Some("Whatup".to_string()),
                    actors: Some(vec!["alice".to_string()]),
                    ..default()
                }),
            ],
        };

        let convo = Conversation::new(raw_talk).unwrap();
        assert_eq!(convo.graph.node_count(), 2);
        assert_eq!(convo.graph.edge_count(), 1);
        assert_eq!(convo.current, NodeIndex::new(0));
    }

    // // 'current_text' tests
    // #[test]
    // fn current_text() {
    //     let raw_talk = RawScript {
    //         actors: vec![],
    //         script: vec![DialogueLine {
    //             id: 1,
    //             text: "Hello".to_string(),
    //             talker: None,
    //             choices: None,
    //             next: None,
    //             start: Some(true),
    //             // end: None,
    //         }],
    //     };

    //     let convo = Conversation::new(raw_talk).unwrap();
    //     assert_eq!(convo.current_text(), "Hello");
    // }

    // // 'next_line' tests
    // #[test]
    // fn next_no_next_err() {
    //     let raw_talk = RawScript {
    //         actors: vec![],
    //         script: vec![DialogueLine {
    //             id: 1,
    //             text: "Hello".to_string(),
    //             talker: None,
    //             choices: None,
    //             next: None,
    //             start: Some(true),
    //             // end: None,
    //         }],
    //     };

    //     let mut convo = Conversation::new(raw_talk).unwrap();
    //     assert_eq!(
    //         convo.next_line().err(),
    //         Some(ConversationError::NoNextDialogue)
    //     );
    // }

    // #[test]
    // fn next_choices_not_handled_err() {
    //     let raw_talk = RawScript {
    //         actors: vec![],
    //         script: vec![
    //             DialogueLine {
    //                 id: 1,
    //                 text: "Hello".to_string(),
    //                 talker: None,
    //                 choices: Some(vec![Choice {
    //                     text: "Whatup".to_string(),
    //                     next: 2,
    //                 }]),
    //                 next: None,
    //                 start: Some(true),
    //                 // end: None,
    //             },
    //             DialogueLine {
    //                 id: 2,
    //                 text: "Whatup to you".to_string(),
    //                 talker: None,
    //                 choices: None,
    //                 next: None,
    //                 start: None,
    //                 // end: None,
    //             },
    //         ],
    //     };

    //     let mut convo = Conversation::new(raw_talk).unwrap();
    //     assert_eq!(
    //         convo.next_line().err(),
    //         Some(ConversationError::ChoicesNotHandled)
    //     );
    // }

    // #[test]
    // fn next_line() {
    //     let raw_talk = RawScript {
    //         actors: vec![],
    //         script: vec![
    //             DialogueLine {
    //                 id: 1,
    //                 text: "Hello".to_string(),
    //                 talker: None,
    //                 choices: None,
    //                 next: Some(2),
    //                 start: Some(true),
    //                 // end: None,
    //             },
    //             DialogueLine {
    //                 id: 2,
    //                 text: "Whatup".to_string(),
    //                 talker: None,
    //                 choices: None,
    //                 next: None,
    //                 start: None,
    //                 // end: None,
    //             },
    //         ],
    //     };

    //     let mut convo = Conversation::new(raw_talk).unwrap();
    //     assert_eq!(convo.current_text(), "Hello");
    //     assert!(convo.next_line().is_ok());
    //     assert_eq!(convo.current_text(), "Whatup");
    // }

    // // 'choices' tests
    // #[test]
    // fn choices_no_choices_err() {
    //     let raw_talk = RawScript {
    //         actors: vec![],
    //         script: vec![DialogueLine {
    //             id: 1,
    //             text: "Hello".to_string(),
    //             talker: None,
    //             choices: None,
    //             next: None,
    //             start: Some(true),
    //             // end: None,
    //         }],
    //     };

    //     let convo = Conversation::new(raw_talk).unwrap();
    //     assert_eq!(convo.choices().err(), Some(ConversationError::NoChoices));
    // }

    // #[test]
    // fn choices() {
    //     let raw_talk = RawScript {
    //         actors: vec![],
    //         script: vec![
    //             DialogueLine {
    //                 id: 1,
    //                 text: "Hello".to_string(),
    //                 talker: None,
    //                 choices: Some(vec![
    //                     Choice {
    //                         text: "Choice 1".to_string(),
    //                         next: 2,
    //                     },
    //                     Choice {
    //                         text: "Choice 2".to_string(),
    //                         next: 3,
    //                     },
    //                 ]),
    //                 next: None,
    //                 start: Some(true),
    //                 // end: None,
    //             },
    //             DialogueLine {
    //                 id: 2,
    //                 text: "Hello".to_string(),
    //                 talker: None,
    //                 choices: None,
    //                 next: Some(3),
    //                 start: None,
    //                 // end: None,
    //             },
    //             DialogueLine {
    //                 id: 3,
    //                 text: "Hello".to_string(),
    //                 talker: None,
    //                 choices: None,
    //                 next: None,
    //                 start: None,
    //                 // end: None,
    //             },
    //         ],
    //     };

    //     let convo = Conversation::new(raw_talk).unwrap();

    //     assert_eq!(convo.choices().unwrap()[0].next, 2);
    //     assert_eq!(convo.choices().unwrap()[1].next, 3);
    //     assert_eq!(convo.choices().unwrap()[0].text, "Choice 1");
    //     assert_eq!(convo.choices().unwrap()[1].text, "Choice 2");
    // }

    // // 'jump_to' tests
    // #[test]
    // fn jump_to_no_line_err() {
    //     let raw_talk = RawScript {
    //         actors: vec![],
    //         script: vec![DialogueLine {
    //             id: 1,
    //             text: "Hello".to_string(),
    //             talker: None,
    //             choices: None,
    //             next: None,
    //             start: Some(true),
    //             // end: None,
    //         }],
    //     };

    //     let mut convo = Conversation::new(raw_talk).unwrap();
    //     assert_eq!(
    //         convo.jump_to(2).err(),
    //         Some(ConversationError::WrongJump(2))
    //     );
    // }
    // #[test]
    // fn jump_to() {
    //     let raw_talk = RawScript {
    //         actors: vec![],
    //         script: vec![
    //             DialogueLine {
    //                 id: 1,
    //                 text: "Hello".to_string(),
    //                 talker: None,
    //                 choices: Some(vec![
    //                     Choice {
    //                         text: "Choice 1".to_string(),
    //                         next: 2,
    //                     },
    //                     Choice {
    //                         text: "Choice 2".to_string(),
    //                         next: 3,
    //                     },
    //                 ]),
    //                 next: None,
    //                 start: Some(true),
    //                 // end: None,
    //             },
    //             DialogueLine {
    //                 id: 2,
    //                 text: "I'm number 2".to_string(),
    //                 talker: None,
    //                 choices: None,
    //                 next: Some(3),
    //                 start: None,
    //                 // end: None,
    //             },
    //             DialogueLine {
    //                 id: 3,
    //                 text: "I;m number 3".to_string(),
    //                 talker: None,
    //                 choices: None,
    //                 next: None,
    //                 start: None,
    //                 // end: None,
    //             },
    //         ],
    //     };

    //     let mut convo = Conversation::new(raw_talk).unwrap();
    //     assert_eq!(convo.current_text(), "Hello");
    //     assert!(convo.jump_to(2).is_ok());
    //     assert_eq!(convo.current_text(), "I'm number 2");
    // }

    // // 'talker_name' tests
    // #[test]
    // fn talker_name_none() {
    //     let raw_talk = RawScript {
    //         actors: vec![],
    //         script: vec![DialogueLine {
    //             id: 1,
    //             text: "Hello".to_string(),
    //             talker: None,
    //             choices: None,
    //             next: None,
    //             start: Some(true),
    //             // end: None,
    //         }],
    //     };

    //     let convo = Conversation::new(raw_talk).unwrap();
    //     assert!(convo.current_talker().is_none());
    // }

    // #[test]
    // fn talker_name() {
    //     let raw_talk = RawScript {
    //         actors: vec![Actor {
    //             name: "Bob".to_string(),
    //             asset: "bob.png".to_string(),
    //         }],
    //         script: vec![DialogueLine {
    //             id: 1,
    //             text: "Hello".to_string(),
    //             talker: Some("Bob".to_string()),
    //             choices: None,
    //             next: None,
    //             start: Some(true),
    //             // end: None,
    //         }],
    //     };

    //     let convo = Conversation::new(raw_talk).unwrap();
    //     assert!(convo.current_talker().is_some());
    // }
}

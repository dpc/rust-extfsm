//! Implementation of a generic final state machine with
//! extended state. Features worth mentioning:
//!
//! * optional exit/enter transitions on states
//! * each event instance can provide boxed arguments to transiton closure
//! * each transition closure can return with vector of arguments that
//!   are queued at the end of outstanding events queue
//! * can generate dot represenation of itself
//!
//! # Author
//! Tony Przygienda, 2016
//!
//! # Examples
//! Check out the tests in the implementation for a good example of use
//!
//! # Panics
//! Never
//!
//! # Errors
//! refer to `Errors`
//!
//! # Copyrights
//!
//! Copyright (c) 2017, Juniper Networks, Inc.
//! All rights reserved.
//!
//! Licensed under the Apache License, Version 2.0 (the "License");
//! you may not use this file except in compliance with the License.
//! This code is not an official Juniper product.
//! You may obtain a copy of the License at
//!
//! http://www.apache.org/licenses/LICENSE-2.0
//!
//! Unless required by applicable law or agreed to in writing, software
//! distributed under the License is distributed on an "AS IS" BASIS,
//! WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
//! See the License for the specific language governing permissions and
//! limitations under the License.

#[macro_use]
extern crate slog;
extern crate dot;
extern crate uuid;
#[macro_use]
extern crate custom_derive;
#[macro_use]
extern crate enum_derive;

use std::collections::{HashMap, VecDeque};
use std::cell::{RefMut, RefCell, Ref};
use std::hash::Hash;
use std::fmt::Debug;
use std::iter::Iterator;
use slog::Logger;
use std::default::Default;
use std::io;
use std::fs;
use uuid::Uuid;

/// types of transitions on states
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub enum EntryExit {
	EntryTransition,
	ExitTransition,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Errors that can occur when running FSMs
pub enum Errors<EventType, StateType, ErrorType> {
	OK,
	/// internal error at a given place that can be generated by transition implementation
	InternalError(EventType, StateType, ErrorType),
	/// the requested transition does not exist, FSM needs to be shut down
	NoTransition(EventType, StateType),
	/// transition failed, you have to shut down the FSM
	TransitionFailure,
}

/// type representing an optional argument to a transition function call
pub type OptionalFnArg<TransitionFnArguments> = Option<Box<TransitionFnArguments>>;

/// set of events to execute with according optional argument on call of transition function
pub type EventQueue<EventType, TransitionFnArguments> =
VecDeque<(EventType, OptionalFnArg<TransitionFnArguments>)>;

/// type to be returned by all transitions
/// an optional queue of events to be added to the FSM or an error is returned
pub type TransitionResult<EventType, StateType, TransitionFnArguments, ErrorType> =
Result<Option<EventQueue<EventType, TransitionFnArguments>>,
	Errors<EventType, StateType, ErrorType>>;

/// transition function used, takes optional argument and returns either with error
/// or an optional set of events to be added to processing (at the end of event queue)
pub type TransitionFn<ExtendedState, EventType, StateType, TransitionFnArguments, ErrorType> =
Fn(RefMut<Box<ExtendedState>>,
	EventType,
	OptionalFnArg<TransitionFnArguments>)
	-> TransitionResult<EventType, StateType, TransitionFnArguments, ErrorType>;

/// transition function to either enter or exit a specific state, return same as
/// `FSMTransitionFn`
pub type EntryExitTransitionFn<ExtendedState,
                               EventType,
                               StateType,
                               TransitionFnArguments,
                               ErrorType> = Fn(RefMut<Box<ExtendedState>>)
	-> TransitionResult<EventType,
		StateType,
		TransitionFnArguments,
		ErrorType>;

/// *Final state machine type*
///
/// # Template parameters
///
///  * `ExtendedState` - provides a structure that every transition can access and
///                      stores extended state
///  * `TransitionFnArguments` - type that can be boxed as parameters to an event instance
///  * `ErrorType` - Errors that transitions can generate internally
pub struct FSM<ExtendedState, StateType, EventType, TransitionFnArguments, ErrorType>
	where StateType: Clone + Eq + Hash + Sized,
	      EventType: Clone + Eq + Hash + Sized
{
	name: String,
	pub extended_state: RefCell<Box<ExtendedState>>,
	current_state: StateType,
	event_queue: EventQueue<EventType, TransitionFnArguments>,
	transitions: TransitionTable<ExtendedState,
		StateType,
		EventType,
		TransitionFnArguments,
		ErrorType>,
	statetransitions: EntryExitTransitionTable<ExtendedState,
		StateType,
		EventType,
		TransitionFnArguments,
		ErrorType>,
	log: Logger,

	/// dotgraph structure for output
	dotgraph: DotGraph<StateType, EventType>,
}

#[derive(Clone, PartialEq, Eq, Hash)]
enum DotEdgeKey<StateType, EventType>
	where StateType: Clone + Sized + Eq + Hash,
	      EventType: Clone + Sized + Eq + Hash
{
	Transition(TransitionSource<StateType, EventType>),
	EntryExit(EntryExitKey<StateType>),
}

/// internal edge to generate DOT graphical view
#[derive(Clone, PartialEq, Eq)]
struct DotEdge<StateType, EventType>
	where StateType: Clone + Sized + Eq + Hash,
	      EventType: Clone + Sized + Eq + Hash
{
	key: DotEdgeKey<StateType, EventType>,
	style: dot::Style,
	label: String,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct DotNodeKey<StateType: Clone + Sized + Eq + Hash>(Option<EntryExit>, StateType);

/// internal node to generate DOT graphical view
#[derive(Clone, PartialEq, Eq)]
struct DotNode<StateType>
	where StateType: Clone + Sized + Eq + Hash
{
	/// None for EntryExit signifies node used for normal transitions
	/// otherwise it's a "shadow node" that does not show up but can be used for
	/// entry-exit transition annotation
	key: DotNodeKey<StateType>,
	id: Uuid,
	shape: Option<String>,
	style: dot::Style,
	label: &'static str,
}

/// graph containing the DOT equivalent of the FSM
struct DotGraph<StateType, EventType>
	where StateType: Clone + Sized + Eq + Hash,
	      EventType: Clone + Sized + Eq + Hash
{
	nodes: HashMap<DotNodeKey<StateType>, DotNode<StateType>>,
	edges: HashMap<DotEdgeKey<StateType, EventType>, DotEdge<StateType, EventType>>,
	id: Uuid,
	/// starting state of FSM
	start_state: Option<StateType>,
}

impl<StateType, EventType> Default for DotGraph<StateType, EventType>
	where StateType: Clone + Sized + Eq + Hash,
	      EventType: Clone + Sized + Eq + Hash
{
	fn default() -> DotGraph<StateType, EventType> {
		DotGraph {
			nodes: HashMap::new(),
			edges: HashMap::new(),
			id: Uuid::new_v4(),
			start_state: None
		}
	}
}

/// graphwalk
impl<'a, ExtendedState, StateType, EventType, TransitionFnArguments, ErrorType>
dot::GraphWalk<'a, DotNodeKey<StateType>, DotEdgeKey<StateType, EventType>>
for FSM<ExtendedState, StateType, EventType, TransitionFnArguments, ErrorType>
	where StateType: Clone + PartialEq + Eq + Hash + Sized,
	      EventType: Clone + PartialEq + Eq + Hash + Sized,
{
	fn nodes(&'a self) -> dot::Nodes<'a, DotNodeKey<StateType>> {
		self.dotgraph.nodes.keys().cloned().collect()
	}

	fn edges(&'a self) -> dot::Edges<'a, DotEdgeKey<StateType, EventType>> {
		self.dotgraph.edges.keys().cloned().collect()
	}

	fn source(&self, e: &DotEdgeKey<StateType, EventType>)
	          -> DotNodeKey<StateType> {
		match e {
			&DotEdgeKey::EntryExit(ref eek) => {
				if eek.1 == EntryExit::EntryTransition {
					DotNodeKey(Some(eek.1.clone()), eek.0.clone())
				} else {
					if let Some(_) = self.statetransitions.get(eek) {
						DotNodeKey(None, eek.0.clone())
					} else {
						unreachable!();
					}
				}
			}
			&DotEdgeKey::Transition(ref tk) => {
				DotNodeKey(None, tk.state.clone())
			}
		}
	}
	fn target(&self, e: &DotEdgeKey<StateType, EventType>)
	          -> DotNodeKey<StateType> {
		// target more tricky, we have to lookup the real table
		match e {
			&DotEdgeKey::EntryExit(ref eek) => {
				if eek.1 == EntryExit::ExitTransition {
					DotNodeKey(Some(eek.1.clone()), eek.0.clone())
				} else {
					if let Some(_) = self.statetransitions.get(eek) {
						DotNodeKey(None, eek.0.clone())
					} else {
						unreachable!();
					}
				}
			}
			&DotEdgeKey::Transition(ref tk) => {
				if let Some(dn) = self.transitions.get(tk) {
					DotNodeKey(None, dn.endstate.clone())
				} else {
					unreachable!();
				}
			}
		}
	}
}

/// graph labelling
impl<'a, ExtendedState, StateType, EventType, TransitionFnArguments, ErrorType>
dot::Labeller<'a, DotNodeKey<StateType>, DotEdgeKey<StateType, EventType>>
for FSM<ExtendedState, StateType, EventType, TransitionFnArguments, ErrorType>
	where StateType: Clone + PartialEq + Eq + Hash + Sized,
	      EventType: Clone + PartialEq + Eq + Hash + Sized,
{
	fn graph_id(&'a self) -> dot::Id<'a> {
		let gid = format!("G{}", self.dotgraph.id.simple());
		dot::Id::new(gid).unwrap()
	}

	fn node_id(&'a self, n: &DotNodeKey<StateType>) -> dot::Id<'a> {
		/// get the node
		match self.dotgraph.nodes.get(n) {
			Some(realnode) => {
				let fid = format!("N{}", realnode.id.simple());
				dot::Id::new(fid).unwrap()
			}
			None => unreachable!(),
		}
	}

	fn node_shape(&'a self, n: &DotNodeKey<StateType>) -> Option<dot::LabelText<'a>> {
		match self.dotgraph.nodes.get(n) {
			Some(realnode) => {
				if let Some(ref r) = realnode.shape {
					Some(dot::LabelText::LabelStr(r.as_str().into()))
				} else {
					Some(dot::LabelText::LabelStr("oval".into()))
				}
			}
			None => unreachable!(),
		}
	}

	fn node_style(&'a self, n: &DotNodeKey<StateType>) -> dot::Style {
		match self.dotgraph.nodes.get(n) {
			Some(realnode) => {
				realnode.style
			}
			None => unreachable!(),
		}
	}

	fn edge_end_arrow(&'a self, _e: &DotEdgeKey<StateType, EventType>) -> dot::Arrow {
		dot::Arrow::normal()
	}

	fn edge_start_arrow(&'a self, _e: &DotEdgeKey<StateType, EventType>) -> dot::Arrow {
		dot::Arrow::none()
	}

	fn edge_style(&'a self, _e: &DotEdgeKey<StateType, EventType>) -> dot::Style {
		dot::Style::None
	}

	fn node_label<'b>(&'b self, n: &DotNodeKey<StateType>) -> dot::LabelText<'b> {
		match self.dotgraph.nodes.get(n) {
			Some(realnode) => {
				dot::LabelText::LabelStr(realnode.label.into())
			}
			None => unreachable!(),
		}
	}

	fn edge_label<'b>(&'b self, ek: &DotEdgeKey<StateType, EventType>) -> dot::LabelText<'b> {
		match self.dotgraph.edges.get(ek) {
			Some(realedge) => {
				dot::LabelText::LabelStr(realedge.label.clone().into())
			}
			None => unreachable!(),
		}
	}
}

/// trait that can process events from a queue using a transition table
pub trait RunsFSM<EventType, StateType, TransitionFnArguments, ErrorType> {
	/// add events to the event queue @ the back, events are _not_ processed
	fn add_events(&mut self,
	              events: &mut Vec<(EventType, OptionalFnArg<TransitionFnArguments>)>)
	              -> Result<u32, Errors<EventType, StateType, ErrorType>>;
	/// process the whole event queue. Observe that this can generate multiple messages
	/// and queue events against the FSM itself again so don't rely which state the machine ends
	/// up in
	///
	/// `returns` - number of events processed or errors encountered.
	///               On errors not much can be done
	///               except killing the FSM instance
	fn process_event_queue(&mut self) -> Result<u32, Errors<EventType, StateType, ErrorType>>;
}

/// implementation of methods to contstruct the machine
impl<ExtendedState, StateType, EventType,
     TransitionFnArguments, ErrorType>
FSM<ExtendedState,
	StateType,
	EventType,
	TransitionFnArguments, ErrorType>
	where StateType: Clone + Eq + Hash + Sized,
	      EventType: Clone + Eq + Hash + Sized,
{
	/// new FSM with an initial extended state box'ed up so it can be passed around easily
	pub fn new(start_state: StateType,
	           extended_init: Box<ExtendedState>,
	           name: &str,
	           log: Logger)
	           -> FSM<ExtendedState, StateType, EventType, TransitionFnArguments, ErrorType> {
		let mut g = DotGraph::default();
		g.start_state = Some(start_state.clone());

		FSM {
			log: log,
			name: String::from(name),
			current_state: start_state,
			event_queue: VecDeque::<(EventType, OptionalFnArg<TransitionFnArguments>)>::new(),
			transitions: TransitionTable::new(),
			statetransitions: EntryExitTransitionTable::new(),
			extended_state: RefCell::new(extended_init),
			dotgraph: g,
		}
	}

	/// provides output of the FSM in dot format
	///
	///   * `filename` - optional filename
	pub fn dotfile(&mut self, filename: Option<String>,
	               state2name: &HashMap<StateType, &'static str>,
	               event2name: &HashMap<EventType, &'static str>,
	) -> Result<(), io::Error> {
		let fileattempt = if let Some(fname) = filename {
			fs::File::create(fname).map(|f| Some(f))
		} else {
			Ok(None)
		};

		if let Ok(maybef) = fileattempt {
			let sout = io::stdout();

			let sv = state2name.keys().cloned().collect::<Vec<_>>();

			// generate the graph, nodes first
			for n in sv.iter() {
				// first _real_ nodes, i.e. not entry/exit
				let key = DotNodeKey(None, n.clone());

				let shape = if let Some(ref sn) = self.dotgraph.start_state {
					if sn == n {
						Some(String::from("diamond"))
					} else {
						None
					}
				} else {
					None
				};

				self.dotgraph.nodes.insert(key.clone(),
				                           DotNode {
					                           key: key,
					                           id: Uuid::new_v4(),
					                           shape: shape,
					                           style: dot::Style::None,
					                           label: state2name.get(n).unwrap_or(&"?"),
				                           }
				);

				// now, let's generate pseudo nodes if necessary with entry, exit with
				// invisible shapes

				for t in
					[EntryExit::EntryTransition,
						EntryExit::ExitTransition]
						.into_iter()
						.collect::<Vec<_>>()
						.into_iter() {
					let eek = (n.clone(), t.clone());

					match self.statetransitions.get(&eek) {
						None => {}
						Some(_) => {
							let label = match t {
								&EntryExit::EntryTransition => { "Enter".into() }
								&EntryExit::ExitTransition => { "Exit".into() }
							};
							let key = DotNodeKey(Some(t.clone()), n.clone());
							self.dotgraph.nodes.insert(key.clone(),
							                           DotNode {
								                           key: key,
								                           id: Uuid::new_v4(),
								                           shape: Some(String::from("plain")),
								                           style: dot::Style::Dashed,
								                           label: label,
							                           });
						}
					}
				}
			}

			// generate the edges now & label them
			for t in self.transitions.iter() {
				let (tk, tv) = t;

				let key = DotEdgeKey::Transition(TransitionSource::new(tk.state.clone(),
				                                                       tk.event.clone()));

				self.dotgraph.edges.insert(key.clone(),
				                           DotEdge {
					                           key: key,
					                           style: dot::Style::None,
					                           label: format!("{}\n|{}|", tv.name.clone()
						                           .unwrap_or(String::from("")),
					                                          event2name.get(&tk.event)
						                                          .unwrap_or(&""))
				                           }
				);
			}

			for t in self.statetransitions.iter() {
				let (tk, tv) = t;

				let key: DotEdgeKey<StateType, EventType> = DotEdgeKey::EntryExit((tk.0.clone(),
				                                                                   tk.1.clone()));

				self.dotgraph.edges.insert(key.clone(),
				                           DotEdge {
					                           key: key,
					                           style: dot::Style::None,
					                           label: format!("{}",
					                                          tv.1.clone().unwrap_or(
						                                          String::from("")))
				                           });
			}

			let render = move |mut mf, mut sout| {
				match &mut mf {
					&mut Some(ref mut f) => dot::render(self, f),
					_ => dot::render(self, &mut sout) // as io::Write
				}
			};

			render(maybef, sout)
		} else {
			Err(fileattempt.err().unwrap()) // error
		}
	}

	/// new transition
	///
	/// `returns` - TRUE if transition has been inserted,
	///             FALSE if a previous has been overwritten!
	pub fn add_transition(&mut self,
	                      from: TransitionSource<StateType, EventType>,
	                      to: TransitionTarget<ExtendedState,
		                      StateType,
		                      EventType,
		                      TransitionFnArguments,
		                      ErrorType>) -> bool {
		self.transitions.insert(from, to).is_none()
	}

	/// new enter/exit transition per state
	/// executed _after_ the transition right before
	/// the state is entered. If the machine remains in the same state
	/// neither the enter nor the exit transitions are called
	///
	/// `returns` - TRUE if transition has been inserted, FALSE if a
	///             previous has been overwritten!
	pub fn add_enter_transition(&mut self, case: (StateType, EntryExit),
	                            trans:
	                            Box<EntryExitTransitionFn<ExtendedState,
		                            EventType,
		                            StateType,
		                            TransitionFnArguments,
		                            ErrorType>>,
	                            name: Option<&str>) -> bool
	{
		self.statetransitions.insert(case,
		                             (trans, name.map(|s| String::from(s)))).is_none()
	}

	pub fn name(&self) -> &String {
		&self.name
	}

	/// gives a read only peek into the extended state from the outside of transitions.
	/// Must be given up before running machine of course
	pub fn extended_state(&self) -> Ref<Box<ExtendedState>> {
		self.extended_state.borrow()
	}

	/// check current state read-only
	pub fn current_state(&self) -> StateType {
		self.current_state.clone()
	}

	/// `returns` - TRUE if machine has outstanding events queued to process
	pub fn events_pending(&self) -> bool {
		self.event_queue.len() > 0
	}
}

/// describes a transition origination point
#[derive(Hash, Eq, PartialEq, Clone)]
pub struct TransitionSource<StateType, EventType> {
	state: StateType,
	event: EventType
}

impl<StateType, EventType>
TransitionSource<StateType, EventType> {
	/// create a transition source
	///   * `state` - original state
	///   * `event` - event occuring
	pub fn new(state: StateType,
	           event: EventType) -> TransitionSource<StateType, EventType> {
		TransitionSource {
			state: state,
			event: event,
		}
	}
}

type EntryExitKey<StateType> = (StateType, EntryExit);

/// implements the target of a transition upon an event
pub struct TransitionTarget<ExtendedState, StateType, EventType,
                            TransitionFnArguments, ErrorType> {
	endstate: StateType,
	transfn: Box<TransitionFn<ExtendedState,
		EventType,
		StateType,
		TransitionFnArguments,
		ErrorType>>,
	name: Option<String>,
}

impl<ExtendedState, StateType, EventType, TransitionFnArguments, ErrorType>
TransitionTarget<ExtendedState, StateType, EventType, TransitionFnArguments, ErrorType>
{
	/// create a transition target
	///   * `endstate` - state resulting after correct transition
	///   * `transfn`  - transition as a boxed function taking in extended state,
	/// 				 event and possible arguments
	///   * `name`     - optional transition name, helpful if `transfn` is a closure
	pub fn new(endstate: StateType,
	           transfn: Box<TransitionFn<ExtendedState,
		           EventType,
		           StateType,
		           TransitionFnArguments,
		           ErrorType>>,
	           name: Option<&str>)
	           -> TransitionTarget
	           <ExtendedState, StateType, EventType, TransitionFnArguments, ErrorType> {
		TransitionTarget {
			endstate: endstate,
			transfn: transfn,
			name: name.map(|s| String::from(s))
		}
	}
}

/// map of from state/event to end state/transition
type TransitionTable<ExtendedState, StateType, EventType, TransitionFnArguments, ErrorType> =
HashMap<// from
	TransitionSource<StateType, EventType>,
	TransitionTarget<ExtendedState,
		StateType,
		EventType,
		TransitionFnArguments,
		ErrorType>>;

/// map for state entry/exit transitions
type EntryExitTransitionTable<ExtendedState,
                              StateType,
                              EventType,
                              TransitionFnArguments,
                              ErrorType> =
HashMap<// from
	EntryExitKey<StateType>,
	// transition
	(Box<EntryExitTransitionFn<ExtendedState,
		EventType,
		StateType,
		TransitionFnArguments,
		ErrorType>>,
	 Option<String>)>;

impl<ExtendedState, EventType, StateType,
     TransitionFnArguments, ErrorType>
RunsFSM<EventType, StateType, TransitionFnArguments, ErrorType>
for FSM<ExtendedState, StateType, EventType,
	TransitionFnArguments, ErrorType>
	where StateType: Clone + PartialEq + Eq + Hash + Debug + Sized,
	      EventType: Clone + PartialEq + Eq + Hash + Debug + Sized,
	      ErrorType: Debug
{
	fn add_events(&mut self,
	              events: &mut Vec<(EventType,
	                                OptionalFnArg<TransitionFnArguments>)>)
	              -> Result<u32, Errors<EventType, StateType, ErrorType>> {
		let el = events.len();

		debug!(self.log, "FSM {} adding {} events", self.name, el);

		// move the queue into the closure and add events
		events.drain(..).map(move |e| {
			self.event_queue.push_back(e);
		}).last();

		Ok(el as u32)
	}

	fn process_event_queue(&mut self) -> Result<u32, Errors<EventType, StateType, ErrorType>> {
		// need to recopy since we will be adding new events on transition possibly
		// so current events need to be frozen
		let mut evs = self.event_queue.drain(..).collect::<Vec<_>>();
		let nrev = evs.len() as u32;

		let mut lr: Vec<Errors<EventType, StateType, ErrorType>> =
			evs.drain(..).map(|e| {
				let state = self.current_state.clone();
				let event = e.0.clone();
				let trans = self.transitions.get(&TransitionSource::new(state.clone(),
				                                                        event.clone()));
				let ref mut q = self.event_queue;
				let name = &self.name;
				debug!(self.log, "FSM {} processing event {:?}/{:?}", name, event, state);

				// play the entry, exit transition draining the event queues if necessary
				fn entryexit<ExtendedState, EventType, StateType,
				             TransitionFnArguments, ErrorType>(
					log: &Logger,
					extstate: RefMut<Box<ExtendedState>>,
					name: &str,
					s: StateType,
					dir: EntryExit,
					q: &mut EventQueue<EventType, TransitionFnArguments>,
					trans: &EntryExitTransitionTable<ExtendedState,
						StateType, EventType,
						TransitionFnArguments, ErrorType>)
					-> Errors<EventType, StateType, ErrorType>
					where StateType: Clone + PartialEq + Eq + Hash + Debug,
					      EventType: Clone + PartialEq + Eq + Hash + Debug,
					      ErrorType: Debug
				{
					match trans.get(&(s.clone(), dir)) {
						None => Errors::OK,
						Some(ref tuple) => {
							let ref func = tuple.0;
							let ref tname = tuple.1;
							debug!(log, "FSM {} exit/entry state transition for {:?} {:?}",
							name, s, tname);
							match func(extstate) {
								Err(v) => v,
								Ok(ref mut v) => {
									match v {
										&mut Some(ref mut eventset) => {
											eventset.drain(..).map(
												|e|
													q.push_back(e)
											).last();
											Errors::OK
										}
										&mut None => Errors::OK,
									}
								}
							}
						}
					}
				}

				match trans {
					Some(itrans) => {
						let endstate = itrans.endstate.clone();
						let transfn = &itrans.transfn;

						let mut res = Errors::OK;

						res = if state == endstate.clone() {
							res
						} else {
							// run exit for state
							let extstate = self.extended_state.borrow_mut();
							entryexit(&self.log,
							          extstate, name, state.clone(),
							          EntryExit::ExitTransition, q, &self.statetransitions)
						};

						// only continue if exit was ok
						res = match res {
							Errors::OK => {
								let extstate = self.extended_state.borrow_mut();
								// match ref mutably the resulting event set of the transition and
								// drain it into our queue back
								match transfn(extstate, e.0, e.1) {
									Err(v) => v,
									Ok(v) => {
										match v {
											None => {}
											Some(eventset) => {
												q.extend(eventset)
											}
										}
										debug!(self.log, "FSM {} moving machine to {:?}",
										name, endstate);
										self.current_state = endstate.clone();
										Errors::OK
									}
								}
							}
							r => r,
						};

						// see whether we have entry into the next one
						match res {
							Errors::OK => {
								if state == endstate.clone() {
									res
								} else {
									let extstate = self.extended_state.borrow_mut();
									entryexit(&self.log,
									          extstate, name, endstate.clone(),
									          EntryExit::EntryTransition, q,
									          &self.statetransitions)
								}
							}
							r => r,
						}
					}
					None =>
						Errors::NoTransition(event, state),
				}
				// check for any errors in the whole transitions of the queue
			}).filter(|e| {
				match *e {
					Errors::OK => false,
					_ => true
				}
			}).take(1).collect::<Vec<_>>(); // try to get first error out if any

		// check whether we got any errors on transitions
		match lr.pop() {
			Some(x) => {
				debug!(self.log, "FSM {} filter on transition failures yields {:?}",
				self.name, &x);
				Err(x)
			}
			_ => Ok(nrev)
		}
	}
}

#[cfg(test)]
mod tests {
	//! small test of a coin machine opening/closing and checking coins
	//! it does check event generation in the transition, extended state,
	//! transitions on state enter/exit and error returns
	extern crate slog;
	extern crate slog_term;
	extern crate slog_atomic;
	extern crate slog_async;

	use std::collections::HashMap;
	use std::hash::Hash;
	use std::cell::RefMut;

	use slog::*;
	use self::slog_atomic::*;

	use super::{FSM, Errors, RunsFSM, EntryExit, TransitionTarget, TransitionSource};
	use std::borrow::Borrow;
	use std;

	#[derive(Debug, Clone)]
	enum StillCoinType {
		Good,
		Bad,
	}

	#[derive(Debug, Clone)]
	enum StillArguments {
		Coin(StillCoinType),
	}

	custom_derive! {
		#[derive(IterVariants(StateVariants), IterVariantNames(StateNames),
			Debug, Clone, Hash, Eq, PartialEq)]
		enum StillStates {
			ClosedWaitForMoney,
			CheckingMoney,
			OpenWaitForTimeOut,
		}
    }

	custom_derive! {
		#[derive(IterVariants(EventVariants), IterVariantNames(EventNames),
			Debug, Clone, Hash, Eq, PartialEq)]
		enum StillEvents {
			GotCoin,
			// needs coin type
			AcceptMoney,
			RejectMoney,
			Timeout,
		}
	}

	#[derive(Debug)]
	enum StillErrors {
		CoinArgumentMissing,
	}

	struct StillExtState {
		coincounter: u32,
		opened: u32,
		closed: u32,
	}

	type CoinStillFSM = FSM<StillExtState, StillStates, StillEvents, StillArguments, StillErrors>;

	fn build_fsm() -> CoinStillFSM {
		let decorator = slog_term::PlainDecorator::new(std::io::stdout());
		let drain = slog_term::CompactFormat::new(decorator).build().fuse();
		let drain = slog_async::Async::new(drain).build().fuse();

		let drain = AtomicSwitch::new(drain);

		// Get a root logger that will log into a given drain.
		let mainlog = Logger::root(LevelFilter::new(drain, Level::Info).fuse(),
		                           o!("version" => env!("CARGO_PKG_VERSION"),));

		let mut still_fsm = FSM::<StillExtState,
			StillStates,
			StillEvents,
			StillArguments,
			StillErrors>::new(StillStates::ClosedWaitForMoney,
		                      Box::new(StillExtState {
			                      coincounter: 0,
			                      opened: 0,
			                      closed: 0,
		                      }),
		                      "coin_still",
		                      mainlog);

		let check_money = move |_extstate: RefMut<Box<StillExtState>>,
		                        _ev: StillEvents, arg: Option<Box<StillArguments>>| {
			match arg {
				None => {
					Err(Errors::InternalError(StillEvents::GotCoin,
					                          StillStates::ClosedWaitForMoney,
					                          StillErrors::CoinArgumentMissing))
				}
				Some(arg) => {
					match (*arg).borrow() {
						&StillArguments::Coin(ref t) => {
							match t {
								&StillCoinType::Good => {
									Ok(Some(vec![(StillEvents::AcceptMoney, None)]
										.into_iter()
										.collect()))
								}
								&StillCoinType::Bad => {
									Ok(Some(vec![(StillEvents::RejectMoney, None)]
										.into_iter()
										.collect()))
								}
							}
						}
					}
				}
			}
		};

		still_fsm.add_transition(TransitionSource::new(StillStates::ClosedWaitForMoney,
		                                               StillEvents::GotCoin),
		                         TransitionTarget::new(StillStates::CheckingMoney,
		                                               Box::new(check_money),
		                                               Some("ProcessCoin")));

		still_fsm.add_transition(TransitionSource::new(StillStates::CheckingMoney,
		                                               StillEvents::RejectMoney),
		                         TransitionTarget::new(StillStates::ClosedWaitForMoney,
		                                               Box::new(|_, _, _| Ok(None)),
		                                               Some("Rejected")));
		still_fsm.add_transition(TransitionSource::new(StillStates::CheckingMoney,
		                                               StillEvents::GotCoin),
		                         TransitionTarget::new(StillStates::CheckingMoney,
		                                               Box::new(|_, _, _| Ok(None)),
		                                               Some("IgnoreAnotherCoin")));
		still_fsm.add_transition(TransitionSource::new(StillStates::CheckingMoney,
		                                               StillEvents::AcceptMoney),
		                         TransitionTarget::new(StillStates::OpenWaitForTimeOut,
		                                               Box::new(|ref mut estate, _, _| {
			                                               estate.coincounter += 1;
			                                               // we count open/close on entry/exit
			                                               Ok(None)
		                                               }),
		                                               Some("Accepted")));
		still_fsm.add_transition(TransitionSource::new(StillStates::OpenWaitForTimeOut,
		                                               StillEvents::GotCoin),
		                         TransitionTarget::new(StillStates::OpenWaitForTimeOut,
		                                               Box::new(|_, _, _| {
			                                               Ok(Some(vec![(StillEvents::RejectMoney,
			                                                             None)]
				                                               .into_iter()
				                                               .collect()))
		                                               }),
		                                               Some("Reject")));
		still_fsm.add_transition(TransitionSource::new(StillStates::OpenWaitForTimeOut,
		                                               StillEvents::RejectMoney),
		                         TransitionTarget::new(StillStates::OpenWaitForTimeOut,
		                                               Box::new(|_, _, _| Ok(None)),
		                                               Some("Rejected")));
		still_fsm.add_transition(TransitionSource::new(StillStates::OpenWaitForTimeOut,
		                                               StillEvents::Timeout),
		                         TransitionTarget::new(StillStates::ClosedWaitForMoney,
		                                               Box::new(|_, _, _| Ok(None)),
		                                               Some("TimeOut")));

		still_fsm.add_enter_transition((StillStates::OpenWaitForTimeOut,
		                                EntryExit::EntryTransition),
		                               Box::new(|ref mut estate| {
			                               estate.opened += 1;
			                               Ok(None)
		                               }),
		                               Some("CountOpens"));
		still_fsm.add_enter_transition((StillStates::OpenWaitForTimeOut,
		                                EntryExit::ExitTransition),
		                               Box::new(|ref mut estate| {
			                               estate.closed += 1;
			                               Ok(None)
		                               }),
		                               Some("CountClose"));

		still_fsm
	}

	#[test]
	fn coin_machine_test() {
		let mut still_fsm = build_fsm();
		// timeout should give no transition error
		assert!(still_fsm.add_events(&mut vec![(StillEvents::Timeout, None)]).unwrap() == 1);
		match still_fsm.process_event_queue() {
			Ok(v) => panic!(format!("failed with {:?} # processed tokens as Ok(_)", v)),
			Err(v) => {
				match v {
					Errors::NoTransition(StillEvents::Timeout,
					                     StillStates::ClosedWaitForMoney) => {
						()
					}
					_ => panic!("failed with wrong FSM error"),
				}
			}
		}

		// that's how we package arguments, we need to clone the coins then
		let goodcoin = Box::new(StillArguments::Coin(StillCoinType::Good));
		let badcoin = Box::new(StillArguments::Coin(StillCoinType::Bad));

		let mut still_fsm = build_fsm();
		assert!(still_fsm.add_events(&mut vec![(StillEvents::GotCoin, Some(goodcoin.clone())),
		                                       (StillEvents::GotCoin, Some(badcoin.clone())),
		                                       (StillEvents::GotCoin, Some(goodcoin.clone())),
		                                       (StillEvents::GotCoin, Some(goodcoin.clone()))])
			.unwrap() == 4);
		while still_fsm.events_pending() {
			assert!(!still_fsm.process_event_queue().is_err());
		}

		assert!(still_fsm.current_state() == StillStates::OpenWaitForTimeOut);

		assert!(still_fsm.add_events(&mut vec![(StillEvents::Timeout, None), ]).unwrap() == 1);
		while still_fsm.events_pending() {
			assert!(!still_fsm.process_event_queue().is_err());
		}

		assert!(still_fsm.current_state() == StillStates::ClosedWaitForMoney);

		let es = still_fsm.extended_state();

		assert!(es.borrow().coincounter == 1);
		assert!(es.borrow().opened == 1);
		assert!(es.borrow().closed == 1);
	}

	fn zipit<ET>(i1: Box<Iterator<Item=ET>>,
	             i2: Box<Iterator<Item=&'static str>>)
	             -> HashMap<ET, &'static str>
		where ET: Sized + Eq + Hash
	{
		i1.zip(i2).collect::<HashMap<_, _>>()
	}

	#[test]
	fn coin_machine_dot() {
		let mut still_fsm = build_fsm();

		still_fsm.dotfile(None,
		                  &zipit(Box::new(StillStates::iter_variants()),
		                         Box::new(StillStates::iter_variant_names())),
		                  &zipit(Box::new(StillEvents::iter_variants()),
		                         Box::new(StillEvents::iter_variant_names())))
			.expect("cannot dotfile");
		still_fsm.dotfile(Some("target/tmp.dot".into()),
		                  &zipit(Box::new(StillStates::iter_variants()),
		                         Box::new(StillStates::iter_variant_names())),
		                  &zipit(Box::new(StillEvents::iter_variants()),
		                         Box::new(StillEvents::iter_variant_names())))
			.expect("cannot dotfile");
	}
}

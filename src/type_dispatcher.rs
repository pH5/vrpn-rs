// Copyright 2018, Collabora, Ltd.
// SPDX-License-Identifier: BSL-1.0
// Author: Ryan A. Pavlik <ryan.pavlik@collabora.com>

use self::RangedId::*;
use bytes::Bytes;
use crate::handler::*;
use crate::types::*;
use crate::{
    constants, determine_id_range, types, Error, GenericMessage, MessageTypeIdentifier, RangedId,
    Result, TypedMessageBody,
};
use std::{
    collections::HashMap,
    fmt,
    hash::{Hash, Hasher},
};

#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd)]
pub enum RegisterMapping<T: BaseTypeSafeId> {
    /// This was an existing mapping with the given ID
    Found(LocalId<T>),
    /// This was a new mapping, which has been registered and received the given ID
    NewMapping(LocalId<T>),
}

impl<T: BaseTypeSafeId> RegisterMapping<T> {
    /// Access the wrapped ID, no matter if it was new or not.
    pub fn get(&self) -> LocalId<T> {
        match self {
            RegisterMapping::Found(v) => *v,
            RegisterMapping::NewMapping(v) => *v,
        }
    }
}

type HandlerHandleInnerType = types::IdType;

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
struct HandlerHandleInner(HandlerHandleInnerType);

impl HandlerHandleInner {
    fn into_handler_handle(self, message_type_filter: Option<LocalId<TypeId>>) -> HandlerHandle {
        HandlerHandle(message_type_filter, self.0)
    }
}

/// A way to refer uniquely to a single added handler in a TypeDispatcher, in case
/// you want to remove it in the future.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct HandlerHandle(Option<LocalId<TypeId>>, HandlerHandleInnerType);

/// Type storing a boxed callback function, an optional sender ID filter,
/// and the unique-per-CallbackCollection handle that can be used to unregister a handler.
struct MsgCallbackEntry {
    handle: HandlerHandleInner,
    pub handler: Box<dyn Handler + Send>,
    pub sender_filter: Option<LocalId<SenderId>>,
}

impl fmt::Debug for MsgCallbackEntry {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("MsgCallbackEntry")
            .field("handle", &self.handle)
            .field("sender_filter", &self.sender_filter)
            .finish()
    }
}

impl MsgCallbackEntry {
    pub fn new(
        handle: HandlerHandleInner,
        handler: Box<dyn Handler + Send>,
        sender_filter: Option<LocalId<SenderId>>,
    ) -> MsgCallbackEntry {
        MsgCallbackEntry {
            handle,
            handler,
            sender_filter,
        }
    }

    /// Invokes the callback with the given msg, if the sender filter (if not None) matches.
    pub fn call<'a>(&mut self, msg: &'a GenericMessage) -> Result<HandlerCode> {
        if id_filter_matches(self.sender_filter, LocalId(msg.header.sender)) {
            self.handler.handle(msg)
        } else {
            Ok(HandlerCode::ContinueProcessing)
        }
    }
}

/// Stores a collection of callbacks with a name, associated with either a message type,
/// or as a "global" handler mapping called for all message types.
#[derive(Debug)]
struct CallbackCollection {
    name: Bytes,
    callbacks: Vec<Option<MsgCallbackEntry>>,
    next_handle: HandlerHandleInnerType,
}

impl CallbackCollection {
    /// Create CallbackCollection instance
    pub fn new(name: Bytes) -> CallbackCollection {
        CallbackCollection {
            name,
            callbacks: Vec::new(),
            next_handle: 0,
        }
    }

    /// Add a callback with optional sender ID filter
    fn add(
        &mut self,
        handler: Box<dyn Handler + Send>,
        sender: Option<LocalId<SenderId>>,
    ) -> Result<HandlerHandleInner> {
        if self.callbacks.len() > types::MAX_VEC_USIZE {
            return Err(Error::TooManyHandlers);
        }
        let handle = HandlerHandleInner(self.next_handle);
        self.callbacks
            .push(Some(MsgCallbackEntry::new(handle, handler, sender)));
        self.next_handle += 1;
        Ok(handle)
    }

    /// Remove a callback
    fn remove(&mut self, handle: HandlerHandleInner) -> Result<()> {
        match self.callbacks.iter().position(|x| {
            x.as_ref()
                .map(|handler| handler.handle == handle)
                .unwrap_or(false)
        }) {
            Some(i) => {
                self.callbacks.remove(i);
                Ok(())
            }
            None => Err(Error::HandlerNotFound),
        }
    }

    /// Call all callbacks (subject to sender filters)
    fn call(&mut self, msg: &GenericMessage) -> Result<()> {
        for entry in &mut self.callbacks.iter_mut() {
            if let Some(unwrapped_entry) = entry {
                if unwrapped_entry.call(msg)? == HandlerCode::RemoveThisHandler {
                    entry.take();
                }
            }
        }
        Ok(())
    }
}

fn message_type_into_index(message_type: TypeId, len: usize) -> Result<usize> {
    match determine_id_range(message_type, len) {
        BelowZero(v) => Err(Error::InvalidId(v)),
        AboveArray(v) => Err(Error::InvalidId(v)),
        InArray(v) => Ok(v as usize),
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct Name(Bytes);

impl Hash for Name {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write(&self.0[..]);
    }
}

/// Structure holding and dispatching generic and message-filtered callbacks.
///
/// Unlike in the mainline C++ code, this does **not** handle "system" message types.
/// The main reason is that they are easiest hard-coded and need to access the endpoint
/// they're operating on, which can be a struggle to get past the borrow checker.
/// Thus, a hard-coded setup simply turns system messages into SystemMessage enum values,
/// which get queued through the Endpoint trait using interior mutability (e.g. with something like mpsc)type_dispatcher
#[derive(Debug)]
pub struct TypeDispatcher {
    /// Index is the local type ID
    types: Vec<CallbackCollection>,
    types_by_name: HashMap<Name, LocalId<TypeId>>,
    generic_callbacks: CallbackCollection,
    /// Index is the local sender ID
    senders: Vec<SenderName>,
    senders_by_name: HashMap<Name, LocalId<SenderId>>,
}

impl Default for TypeDispatcher {
    fn default() -> TypeDispatcher {
        TypeDispatcher::new()
    }
}

impl TypeDispatcher {
    pub fn new() -> TypeDispatcher {
        let mut disp = TypeDispatcher {
            types: Vec::new(),
            types_by_name: HashMap::new(),
            generic_callbacks: CallbackCollection::new(Bytes::from_static(constants::GENERIC)),
            senders: Vec::new(),
            senders_by_name: HashMap::new(),
        };

        disp.register_sender(constants::CONTROL)
            .expect("couldn't register CONTROL sender");
        disp.register_type(constants::GOT_FIRST_CONNECTION)
            .expect("couldn't register GOT_FIRST_CONNECTION type");
        disp.register_type(constants::GOT_CONNECTION)
            .expect("couldn't register GOT_FIRST_CONNECTION type");
        disp.register_type(constants::DROPPED_CONNECTION)
            .expect("couldn't register DROPPED_CONNECTION type");
        disp.register_type(constants::DROPPED_LAST_CONNECTION)
            .expect("couldn't register DROPPED_LAST_CONNECTION type");
        disp
    }

    /// Get a mutable borrow of the CallbackCollection associated with the supplied TypeId
    /// (or the generic callbacks for None)
    fn get_type_callbacks_mut<'a>(
        &'a mut self,
        type_id_filter: Option<LocalId<TypeId>>,
    ) -> Result<&'a mut CallbackCollection> {
        match type_id_filter {
            Some(i) => {
                let index = message_type_into_index(i.into_id(), self.types.len())?;
                Ok(&mut self.types[index])
            }
            None => Ok(&mut self.generic_callbacks),
        }
    }

    fn add_type(&mut self, name: impl Into<TypeName>) -> Result<LocalId<TypeId>> {
        if self.types.len() > MAX_VEC_USIZE {
            return Err(Error::TooManyMappings);
        }
        let name = name.into();
        self.types.push(CallbackCollection::new(name.clone().0));
        let id = LocalId(TypeId((self.types.len() - 1) as IdType));
        self.types_by_name.insert(Name(name.0), id);
        Ok(id)
    }

    fn add_sender(&mut self, name: impl Into<SenderName>) -> Result<LocalId<SenderId>> {
        if self.senders.len() > (IdType::max_value() - 2) as usize {
            return Err(Error::TooManyMappings);
        }
        let name = name.into();
        self.senders.push(name.clone());
        let id = LocalId(SenderId((self.senders.len() - 1) as IdType));
        self.senders_by_name.insert(Name(name.0), id);
        Ok(id)
    }

    /// Returns the ID for the type name, if found.
    pub fn get_type_id<T>(&self, name: T) -> Option<LocalId<TypeId>>
    where
        T: Into<TypeName>,
    {
        let name: TypeName = name.into();
        let name: Bytes = name.into();
        self.types_by_name.get(&Name(name)).cloned()
    }

    /// Calls add_type if get_type_id() returns None.
    /// Returns the corresponding TypeId in all cases.
    pub fn register_type(&mut self, name: impl Into<TypeName>) -> Result<RegisterMapping<TypeId>> {
        let name: TypeName = name.into();
        match self.get_type_id(name.clone()) {
            Some(i) => Ok(RegisterMapping::Found(i)),
            None => self.add_type(name).map(RegisterMapping::NewMapping),
        }
    }

    /// Calls add_sender if get_sender_id() returns None.
    pub fn register_sender(
        &mut self,
        name: impl Into<SenderName>,
    ) -> Result<RegisterMapping<SenderId>> {
        let name: SenderName = name.into();
        match self.get_sender_id(name.clone()) {
            Some(i) => Ok(RegisterMapping::Found(i)),
            None => self.add_sender(name).map(RegisterMapping::NewMapping),
        }
    }

    /// Returns the ID for the sender name, if found.
    pub fn get_sender_id(&self, name: impl Into<SenderName>) -> Option<LocalId<SenderId>> {
        let name: SenderName = name.into();
        let name: Bytes = name.into();
        self.senders_by_name.get(&Name(name)).cloned()
    }

    pub fn add_handler(
        &mut self,
        handler: Box<dyn Handler + Send>,
        message_type_filter: Option<LocalId<TypeId>>,
        sender_filter: Option<LocalId<SenderId>>,
    ) -> Result<HandlerHandle> {
        self.get_type_callbacks_mut(message_type_filter)?
            .add(handler, sender_filter)
            .map(|h| h.into_handler_handle(message_type_filter))
    }
    pub fn add_typed_handler<T: 'static>(
        &mut self,
        handler: Box<T>,
        sender_filter: Option<LocalId<SenderId>>,
    ) -> Result<HandlerHandle>
    where
        T: TypedHandler + Handler + Sized,
    {
        let message_type = match T::Item::MESSAGE_IDENTIFIER {
            MessageTypeIdentifier::UserMessageName(name) => self.register_type(name)?.get(),
            MessageTypeIdentifier::SystemMessageId(id) => LocalId(id),
        };
        self.add_handler(handler, Some(message_type), sender_filter)
    }

    pub fn remove_handler(&mut self, handler_handle: HandlerHandle) -> Result<()> {
        let HandlerHandle(message_type, inner) = handler_handle;
        self.get_type_callbacks_mut(message_type)?
            .remove(HandlerHandleInner(inner))
    }

    /// Akin to vrpn_TypeDispatcher::doCallbacksFor
    pub fn call(&mut self, msg: &GenericMessage) -> Result<()> {
        let index = message_type_into_index(msg.header.message_type, self.types.len())?;
        let mapping = &mut self.types[index];

        self.generic_callbacks.call(&msg)?;
        mapping.call(&msg)
    }

    pub fn senders_iter<'a>(
        &'a self,
    ) -> impl Iterator<Item = (LocalId<SenderId>, &'a SenderName)> + 'a {
        self.senders
            .iter()
            .enumerate()
            .map(|(i, name)| (LocalId(SenderId(i as i32)), name))
    }
    pub fn types_iter<'a>(&'a self) -> impl Iterator<Item = (LocalId<TypeId>, TypeName)> + 'a {
        self.types
            .iter()
            .enumerate()
            .map(|(i, callbacks)| (LocalId(TypeId(i as i32)), TypeName(callbacks.name.clone())))
    }
}
#[cfg(test)]
mod tests {
    use crate::type_dispatcher::*;
    use crate::{GenericBody, GenericMessage, TimeVal};
    use std::sync::{Arc, Mutex};

    #[derive(Debug, Clone)]
    struct SetTo10 {
        val: Arc<Mutex<i8>>,
    }
    impl Handler for SetTo10 {
        fn handle(&mut self, _msg: &GenericMessage) -> Result<HandlerCode> {
            let mut val = self.val.lock()?;
            *val = 10;
            Ok(HandlerCode::ContinueProcessing)
        }
    }
    #[derive(Debug, Clone)]
    struct SetTo15 {
        val: Arc<Mutex<i8>>,
    }
    impl Handler for SetTo15 {
        fn handle(&mut self, _msg: &GenericMessage) -> Result<HandlerCode> {
            let mut val = self.val.lock()?;
            *val = 15;
            Ok(HandlerCode::ContinueProcessing)
        }
    }
    #[test]
    fn callback_collection() {
        let val: Arc<Mutex<i8>> = Arc::new(Mutex::new(5));
        let a = Arc::clone(&val);
        let sample_callback = SetTo10 { val: a };
        let b = Arc::clone(&val);
        let sample_callback2 = SetTo15 { val: b };

        let mut collection = CallbackCollection::new(Bytes::from_static(b"dummy"));
        let handler = collection
            .add(Box::new(sample_callback.clone()), None)
            .unwrap();
        let msg = GenericMessage::new(
            Some(TimeVal::get_time_of_day()),
            TypeId(0),
            SenderId(0),
            GenericBody::default(),
        );
        collection.call(&msg).unwrap();
        assert_eq!(*val.lock().unwrap(), 10);

        collection
            .remove(handler)
            .expect("Can't remove added callback");
        // No callbacks should fire now.
        *val.lock().unwrap() = 5;
        collection.call(&msg).unwrap();
        assert_eq!(*val.lock().unwrap(), 5);

        let _ = collection
            .add(Box::new(sample_callback2), Some(LocalId(SenderId(0))))
            .unwrap();
        *val.lock().unwrap() = 5;
        collection.call(&msg).unwrap();
        assert_eq!(*val.lock().unwrap(), 15);

        // Check that later-registered callbacks get run later
        let _ = collection.add(Box::new(sample_callback), None).unwrap();
        *val.lock().unwrap() = 5;
        collection.call(&msg).unwrap();
        assert_eq!(*val.lock().unwrap(), 10);

        // This shouldn't trigger callback 2
        let mut msg2 = msg.clone();
        msg2.header.sender = SenderId(1);
        *val.lock().unwrap() = 5;
        collection.call(&msg2).unwrap();
        assert_eq!(*val.lock().unwrap(), 10);
    }

    #[test]
    fn type_dispatcher() {
        let val: Arc<Mutex<i8>> = Arc::new(Mutex::new(5));
        let a = Arc::clone(&val);
        let sample_callback = SetTo10 { val: a };
        let b = Arc::clone(&val);
        let sample_callback2 = SetTo15 { val: b };

        let mut dispatcher = TypeDispatcher::new();
        let handler = dispatcher
            .add_handler(Box::new(sample_callback.clone()), None, None)
            .unwrap();
        let msg = GenericMessage::new(
            Some(TimeVal::get_time_of_day()),
            TypeId(0),
            SenderId(0),
            GenericBody::default(),
        );
        dispatcher.call(&msg).unwrap();
        assert_eq!(*val.lock().unwrap(), 10);

        dispatcher
            .remove_handler(handler)
            .expect("Can't remove added callback");
        // No callbacks should fire now.
        *val.lock().unwrap() = 5;
        dispatcher.call(&msg).unwrap();
        assert_eq!(*val.lock().unwrap(), 5);

        let _ = dispatcher
            .add_handler(Box::new(sample_callback2), None, Some(LocalId(SenderId(0))))
            .unwrap();
        *val.lock().unwrap() = 5;
        dispatcher.call(&msg).unwrap();
        assert_eq!(*val.lock().unwrap(), 15);

        // Check that later-registered callbacks get run later
        let _ = dispatcher
            .add_handler(Box::new(sample_callback), None, None)
            .unwrap();
        *val.lock().unwrap() = 5;
        dispatcher.call(&msg).unwrap();
        assert_eq!(*val.lock().unwrap(), 10);

        // This shouldn't trigger callback 2
        let mut msg2 = msg.clone();
        msg2.header.sender = SenderId(1);
        *val.lock().unwrap() = 5;
        dispatcher.call(&msg2).unwrap();
        assert_eq!(*val.lock().unwrap(), 10);
    }
}

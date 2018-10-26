// Copyright 2018, Collabora, Ltd.
// SPDX-License-Identifier: BSL-1.0
// Author: Ryan A. Pavlik <ryan.pavlik@collabora.com>

use vrpn::connection::Endpoint;
use vrpn::constants;
use vrpn::typedispatcher::HandlerResult;
use vrpn::types::*;
use vrpn::TranslationTable;
extern crate bytes;
use self::bytes::BufMut;
use self::bytes::BytesMut;
use std::fmt::Write;

struct OutputBuf {}
impl OutputBuf {
    fn new() -> OutputBuf {
        OutputBuf {}
    }

    fn pack_description<T: BaseTypeSafeId>(
        &mut self,
        table: &mut TranslationTable<T>,
        local_id: LocalId<T>,
        sender: SenderId,
    ) {
        let entry = table.get_by_local_id(local_id).unwrap();
        let length = entry.name.len() + 1; // + 1 is for null-terminator.
        let mut buf = bytes::BytesMut::with_capacity(length);
        buf.put_u32_be(length as u32);
        write!(buf, "{}\0", entry.name);
        println!("{:?}", &buf);
    }
}
pub struct EndpointIP {
    types: TranslationTable<TypeId>,
    senders: TranslationTable<SenderId>,
    output: OutputBuf,
}

impl EndpointIP {
    pub fn new() -> EndpointIP {
        EndpointIP {
            types: TranslationTable::new(),
            senders: TranslationTable::new(),
            output: OutputBuf::new(),
        }
    }
}
impl Endpoint for EndpointIP {
    fn send_message(
        &mut self,
        time: Time,
        message_type: TypeId,
        sender: SenderId,
        buffer: bytes::Bytes,
        class: ClassOfService,
    ) -> HandlerResult<()> {
        /// @todo
        Ok(())
    }

    fn local_type_id(&self, remote_type: RemoteId<TypeId>) -> Option<LocalId<TypeId>> {
        match self.types.map_to_local_id(remote_type) {
            Ok(val) => val,
            Err(_) => None,
        }
    }
    fn local_sender_id(&self, remote_sender: RemoteId<SenderId>) -> Option<LocalId<SenderId>> {
        match self.senders.map_to_local_id(remote_sender) {
            Ok(val) => val,
            Err(_) => None,
        }
    }
    fn new_local_sender(&mut self, name: &'static str, local_sender: LocalId<SenderId>) -> bool {
        self.senders.add_local_id(name, local_sender)
    }
    fn new_local_type(&mut self, name: &'static str, local_type: LocalId<TypeId>) -> bool {
        self.types.add_local_id(name, local_type)
    }

    fn pack_sender_description(&mut self, local_sender: LocalId<SenderId>) {
        self.output.pack_description::<SenderId>(
            &mut self.senders,
            local_sender,
            constants::SENDER_DESCRIPTION,
        );
    }

    fn pack_type_description(&mut self, local_type: LocalId<TypeId>) {
        self.output.pack_description::<TypeId>(
            &mut self.types,
            local_type,
            constants::TYPE_DESCRIPTION,
        );
    }
}

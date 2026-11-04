use std::io::{self, IoSlice, IoSliceMut};
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};

use rustix::net::{
    AddressFamily, RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, SendAncillaryBuffer,
    SendAncillaryMessage, SendFlags, SocketFlags, SocketType, recvmsg, sendmsg, socketpair,
};
use serde::{Deserialize, Serialize};

use crate::plan::VmProcessPlan;
use crate::probe::Attempt;
use crate::sandbox::Restrictions;

pub const MAXIMUM_INSTRUCTIONS_BYTES: usize = 1024 * 1024;
pub const MAXIMUM_DESCRIPTORS: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Work {
    Boot { plan: Box<VmProcessPlan> },
    Receive { plan: Box<VmProcessPlan> },
    Probe { attempt: Attempt },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Instructions {
    pub restrictions: Restrictions,
    pub work: Work,
}

pub struct ControlChannel {
    pub parent: OwnedFd,
    pub child: OwnedFd,
}

impl ControlChannel {
    pub fn new() -> io::Result<Self> {
        let (parent, child) =
            socketpair(AddressFamily::UNIX, SocketType::SEQPACKET, SocketFlags::CLOEXEC, None)?;
        Ok(Self { parent, child })
    }
}

pub fn send(
    socket: impl AsFd,
    instructions: &Instructions,
    descriptors: &[BorrowedFd<'_>],
) -> io::Result<()> {
    let bytes = serde_json::to_vec(instructions).map_err(io::Error::other)?;
    if bytes.len() > MAXIMUM_INSTRUCTIONS_BYTES || descriptors.len() > MAXIMUM_DESCRIPTORS {
        return Err(io::Error::other("the VM process instructions are too large"));
    }
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(MAXIMUM_DESCRIPTORS))];
    let mut ancillary = SendAncillaryBuffer::new(&mut space);
    if !descriptors.is_empty() && !ancillary.push(SendAncillaryMessage::ScmRights(descriptors)) {
        return Err(io::Error::other("the descriptors do not fit in one message"));
    }
    let sent = sendmsg(socket, &[IoSlice::new(&bytes)], &mut ancillary, SendFlags::empty())?;
    if sent == bytes.len() { Ok(()) } else { Err(io::Error::other("the instructions were truncated")) }
}

pub fn receive(socket: impl AsFd) -> io::Result<(Instructions, Vec<OwnedFd>)> {
    let mut bytes = vec![0_u8; MAXIMUM_INSTRUCTIONS_BYTES];
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(MAXIMUM_DESCRIPTORS))];
    let mut ancillary = RecvAncillaryBuffer::new(&mut space);
    let received =
        recvmsg(socket, &mut [IoSliceMut::new(&mut bytes)], &mut ancillary, RecvFlags::CMSG_CLOEXEC)?;
    let mut descriptors = Vec::new();
    for message in ancillary.drain() {
        if let RecvAncillaryMessage::ScmRights(received_descriptors) = message {
            descriptors.extend(received_descriptors);
        }
    }
    if received.flags.contains(rustix::net::ReturnFlags::TRUNC) {
        return Err(io::Error::other("the instructions were truncated"));
    }
    let instructions = serde_json::from_slice(&bytes[..received.bytes]).map_err(io::Error::other)?;
    Ok((instructions, descriptors))
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Seek, Write};

    use super::*;

    #[test]
    fn instructions_and_descriptors_cross_the_channel() {
        let channel = ControlChannel::new().unwrap();
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(b"shared").unwrap();
        let instructions = Instructions {
            restrictions: Restrictions { readable: vec!["/boot".into()], writable: vec!["/run/vm".into()] },
            work: Work::Probe { attempt: Attempt::OpenInternetSocket },
        };
        send(&channel.parent, &instructions, &[file.as_fd()]).unwrap();
        let (received, descriptors) = receive(&channel.child).unwrap();
        assert_eq!(received, instructions);
        assert_eq!(descriptors.len(), 1);
        let mut shared = std::fs::File::from(descriptors.into_iter().next().unwrap());
        shared.rewind().unwrap();
        let mut content = String::new();
        shared.read_to_string(&mut content).unwrap();
        assert_eq!(content, "shared");
    }
}

use std::io::{self, IoSlice, Read, Write};
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use rustix::net::{SendAncillaryBuffer, SendAncillaryMessage, SendFlags, sendmsg};
use serde_json::json;
use vmm::external_fds::Target;

use crate::control::MAXIMUM_DESCRIPTORS;
use crate::migration::MIGRATION_TIMEOUT_SECONDS;
use crate::vmm_api;

const ANSWER_MARGIN_SECONDS: u64 = 60;

#[must_use]
pub fn receive_request(receiver_url: &str, interfaces: &[String]) -> String {
    let external_fds: Vec<_> = interfaces
        .iter()
        .map(|id| json!({"target": Target::Net { id: id.clone() }, "expected_fds": 1}))
        .collect();
    json!({"receiver_url": receiver_url, "external_fds": external_fds}).to_string()
}

fn send_with_descriptors(
    stream: &UnixStream,
    bytes: &[u8],
    descriptors: &[BorrowedFd<'_>],
) -> io::Result<()> {
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(MAXIMUM_DESCRIPTORS))];
    let mut ancillary = SendAncillaryBuffer::new(&mut space);
    if !descriptors.is_empty() && !ancillary.push(SendAncillaryMessage::ScmRights(descriptors)) {
        return Err(io::Error::other("the taps do not fit in one message"));
    }
    let sent = sendmsg(stream, &[IoSlice::new(bytes)], &mut ancillary, SendFlags::empty())?;
    let mut remaining = stream;
    remaining.write_all(&bytes[sent..])
}

fn read_response(stream: &mut UnixStream) -> io::Result<vmm_api::Response> {
    let mut received = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        if let Some(response) = vmm_api::complete(&received) {
            return Ok(response);
        }
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            return vmm_api::closed(&received);
        }
        received.extend_from_slice(&chunk[..read]);
    }
}

pub fn receive_migration(
    api_socket: &Path,
    receiver_url: &str,
    interfaces: &[String],
    taps: &[OwnedFd],
) -> io::Result<()> {
    if interfaces.len() != taps.len() {
        return Err(io::Error::other(format!("{} interfaces but {} taps", interfaces.len(), taps.len())));
    }
    let mut stream = UnixStream::connect(api_socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(MIGRATION_TIMEOUT_SECONDS + ANSWER_MARGIN_SECONDS)))?;
    let request =
        vmm_api::put_request(vmm_api::RECEIVE_MIGRATION, &receive_request(receiver_url, interfaces));
    let descriptors: Vec<BorrowedFd<'_>> = taps.iter().map(AsFd::as_fd).collect();
    send_with_descriptors(&stream, request.as_bytes(), &descriptors)?;
    let response = read_response(&mut stream)?;
    if response.is_success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "the VMM refused to receive the VM ({}): {}",
            response.status, response.body
        )))
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::os::unix::net::UnixListener;

    use rustix::net::{RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, recvmsg};

    use super::*;

    #[test]
    fn each_interface_asks_for_one_tap_by_its_device_id() {
        let request: serde_json::Value = serde_json::from_str(&receive_request(
            "unix:/run/incoming",
            &[String::from("net0"), String::from("net1")],
        ))
        .unwrap();
        assert_eq!(request["receiver_url"], "unix:/run/incoming");
        assert_eq!(
            request["external_fds"][1]["target"],
            serde_json::to_value(Target::Net { id: String::from("net1") }).unwrap()
        );
        assert_eq!(request["external_fds"][0]["expected_fds"], 1);
    }

    #[test]
    fn the_taps_travel_with_the_request_and_the_answer_is_read() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("api.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let vmm = std::thread::spawn(move || {
            let (mut connection, _) = listener.accept().unwrap();
            let mut bytes = vec![0_u8; 4096];
            let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(MAXIMUM_DESCRIPTORS))];
            let mut ancillary = RecvAncillaryBuffer::new(&mut space);
            let received = recvmsg(
                &connection,
                &mut [io::IoSliceMut::new(&mut bytes)],
                &mut ancillary,
                RecvFlags::empty(),
            )
            .unwrap();
            let descriptors: usize = ancillary
                .drain()
                .map(|message| match message {
                    RecvAncillaryMessage::ScmRights(received) => received.count(),
                    _ => 0,
                })
                .sum();
            let request = String::from_utf8_lossy(&bytes[..received.bytes]).into_owned();
            connection.write_all(b"HTTP/1.1 204 No Content\r\n\r\n").unwrap();
            let mut rest = Vec::new();
            let _ = connection.read_to_end(&mut rest);
            (request, descriptors)
        });
        let (first, _) = std::io::pipe().unwrap();
        let (second, _) = std::io::pipe().unwrap();
        let taps = [OwnedFd::from(first), OwnedFd::from(second)];
        receive_migration(
            &socket,
            "unix:/run/incoming",
            &[String::from("net0"), String::from("net1")],
            &taps,
        )
        .unwrap();
        let (request, descriptors) = vmm.join().unwrap();
        assert!(request.starts_with("PUT /api/v1/vm.receive-migration HTTP/1.1"));
        assert_eq!(descriptors, 2);
    }
}

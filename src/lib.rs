use serde::{Deserialize, Serialize};
use std::fmt;
use std::fmt::format;
use std::string::ToString;
use std::sync::Arc;
use serde_json::to_vec;

use tokio::{
    io::{self, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    signal, sync,
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

pub type GenericError = Box<dyn std::error::Error + Send + Sync + 'static>;
pub type Result<T> = std::result::Result<T, GenericError>;

pub static SERVER_ADDR: &str = "127.0.0.1:6440";
pub const MAX_NUM_USERS: usize = 2000;

pub const INCOMING_BUFFER_LEN: usize =
    Message::MAX_USERNAME_LEN * 10 + Message::MAX_CONTENT_LEN * 10 + 200;

/// SharedIdPool is a tuple struct type which wraps a single Arc<std::sync::Mutex<Vec<usize>>>. A
/// possible feature to add would be some kind of mutex sharding, so that multiple clients can
/// connect and disconnect at the same time.
pub struct SharedIdPool(Arc<std::sync::Mutex<Vec<usize>>>);

impl SharedIdPool {
    pub fn new() -> SharedIdPool {
        assert!(MAX_NUM_USERS < usize::MAX);

        let id_pool = (1..=MAX_NUM_USERS).rev().collect();
        SharedIdPool(Arc::new(std::sync::Mutex::new(id_pool)))
    }

    pub fn clone(&self) -> SharedIdPool {
        SharedIdPool(self.0.clone())
    }

    pub fn obtain_id_token(&self) -> Result<usize> {
        let lock_result = self.0.lock();
        let mut locked_vec = match lock_result {
            Ok(t) => t,
            Err(_) => return Err(GenericError::from("The mutex of the id pool was poisoned.")),
        };
        locked_vec
            .pop()
            .ok_or(GenericError::from("No ids left in the id pool."))
    }

    pub fn redeem_token(&self, id: usize) -> Result<()> {
        let lock_result = self.0.lock();
        let mut locked_vec = match lock_result {
            Ok(t) => t,
            Err(_) => return Err(GenericError::from("The mutex of the id pool was poisoned.")),
        };
        locked_vec.push(id);
        Ok(())
    }
}

/// Message is the type of packet unit that the client uses. They comprehend a username of at most
/// MAX_USERNAME_LEN bytes and a content of at most MAX_CONTENT_LEN bytes.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Message {
    username: String,
    content: String,
}

pub enum UserStatus {
    Present,
    Absent,
}

impl Message {
    /// Maximal length (in bytes) of the username.
    pub const MAX_USERNAME_LEN: usize = 30 * 4;

    /// Maximal length (in bytes) of the content of the message.
    pub const MAX_CONTENT_LEN: usize = 256 * 4;

    pub fn new(username: &str, content: &str) -> Result<Message> {
        if username.as_bytes().len() > Message::MAX_USERNAME_LEN {
            let error_message = format!(
                "The username '{username}' is too long, it must be at most {} bytes long!",
                Message::MAX_USERNAME_LEN
            );
            return Err(GenericError::from(error_message));
        }

        if content.as_bytes().len() > Message::MAX_CONTENT_LEN {
            let error_message = format!(
                "The content is too long, it must be at most {} bytes long!",
                Message::MAX_CONTENT_LEN
            );
            return Err(GenericError::from(error_message));
        }

        if content.contains(TcpPakket::INIT_DELIMITER as char) || content.contains(TcpPakket::END_DELIMITER as char) {
            let error_message = format!("The symbols '{}' and '{}' are not allowed in the messages!", TcpPakket::INIT_DELIMITER as char, TcpPakket::END_DELIMITER as char);
            return Err(GenericError::from(error_message));
        }

        Ok(Message {
            username: username.to_string(),
            content: content.to_string(),
        })
    }

    pub fn get_username(&self) -> String {
        self.username.clone()
    }

    pub fn get_content(&self) -> String {
        self.content.clone()
    }

    pub fn serialized(&self) -> Result<String> {
        let serialized = serde_json::to_string(&self)?;
        Ok(serialized)
    }

    pub fn craft_status_change_msg(username: &str, status: UserStatus) -> Message {
        let content = match status {
            UserStatus::Present => format!("{username} just joined the chat"),
            UserStatus::Absent => format!("{username} just left the chat"),
        };

        Message {
            username: "SERVER".to_string(),
            content,
        }
    }

    pub fn craft_server_interrupt_msg() -> Message {
        let content = "The SERVER is shutting down this connection.".to_string();

        Message {
            username: "SERVER".to_string(),
            content,
        }
    }
}

impl TryFrom<TcpPakket> for Message {
    type Error = GenericError;
    fn try_from(value: TcpPakket) -> Result<Self> {
        let serialized = value.0[1..(value.0.len() - 1)].to_string();
        let deserialized_msg = serde_json::from_str::<Message>(&serialized)?;
        Ok(deserialized_msg)
    }
}

/// TcpPakket is a String delimited by the char TcpPakket::DELIMITER and passed through tcp channels.
/// It is intended as a way to pass serialized messages, but nothing ensures that the String
/// embedded is a valid serialized object of any kind.
#[derive(Debug, PartialEq, Clone)]
pub struct TcpPakket(String);

impl TcpPakket {
    /// Such delimiters are a single char marking the beginning and end of TcpPakkets. Notice that it
    /// is fundamental for them to be ascii values (u8) and to be different from each others.
    const INIT_DELIMITER: u8 = b'|';
    const END_DELIMITER: u8 = b'`';
    const VALID: bool = TcpPakket::INIT_DELIMITER.is_ascii() && TcpPakket::END_DELIMITER.is_ascii() && (TcpPakket::INIT_DELIMITER != TcpPakket::END_DELIMITER);

    pub fn new_to_pak(string: &str) -> TcpPakket {
        if !TcpPakket::VALID {
            panic!();
        }

        let pakket = format!(
            "{}{string}{}",
            TcpPakket::INIT_DELIMITER as char,
            TcpPakket::END_DELIMITER as char
        );
        TcpPakket(pakket)
    }

    pub fn new_pakked(pakket_str: &str) -> Result<TcpPakket> {
        if !TcpPakket::VALID {
            panic!();
        }

        if pakket_str.chars().first() != Some(TcpPakket::INIT_DELIMITER as char) || pakket_str.chars().last() != Some(TcpPakket::END_DELIMITER as char) {
            let err_msg = "This string does not have the correct delimiters to be a pakket.".to_string();
            return Err(GenericError::from(err_msg));
        }
        Ok(TcpPakket(pakket_str.to_string()))
    }

    pub fn unpack(mut self) -> String {
        let length = self.0.len();
        self.0[1..length - 1].to_string()
    }
}

/// This function takes a reader and a sender of an mpsc channel and, with a buffer, processes
/// the incoming messages. Terminology:
/// 1. Shipment = the amount of data readable from the moment in which the reader starts to return
///     Ok(positive number) till the moment in which the reader waits for more data.
/// 2. Wave = the part of shipment which is loaded on the buffer
pub async fn process_incoming<Rd>(
    reader: &mut Rd,
    sender: sync::mpsc::Sender<Message>,
) -> Result<()>
where
    Rd: AsyncRead + Unpin,
{
    let init_delimiter: u8 = TcpPakket::INIT_DELMITER;
    let end_delimiter: u8 = TcpPakket::END_DELMITER;

    let mut wave_buffer: Vec<u8> = Vec::with_capacity(INCOMING_BUFFER_LEN);
    let mut previous_fragment: Vec<u8> = Vec::with_capacity(INCOMING_BUFFER_LEN);

    // todo: add the cancellation token and the tokio::select!
    // todo: handle this errors without exiting the function
    'process_connection: loop {
        wave_buffer.clear();
        // read_buf returns till there is a shipment
        match reader.read_buf(&mut wave_buffer).await {
            Ok(n) if n > 0 => {
                let mut messages: Vec<Message> = Vec::with_capacity(
                    INCOMING_BUFFER_LEN / (Message::MAX_CONTENT_LEN + Message::MAX_USERNAME_LEN),
                );

                let mut fragments: Vec<&[u8]> = wave_buffer.split_inclusive(|&byte| byte == end_delimiter).collect();

                let (first_frag, other_frags) = match fragments.split_first() {
                    Some(smt) => (*smt.0, smt.1),
                    None => {
                        let err_msg = "The wave buffer is empty even though the reader read  a non-null amount of bytes!".to_string();
                        return Err(GenericError::from(err_msg));
                    },
                };

                // process first fragments
                let first_msg = if first_frag.len() {
                    let err_msg = "The first fragment is 0 bytes long!".to_string();
                    return Err(GenericError::from(err_msg));
                } else if first_frag.first() != Some(&init_delimiter) {
                    let first_pakked = format!("{}{}", String::from_utf8(previous_fragment.to_vec())?, String::from_utf8(first_frag.into_vec())?);
                    previous_fragment.clear();
                    let first_msg = Message::try_from(TcpPakket::new_pakked(&first_pakked)?)?;
                    first_msg
                } else {
                    let first_pakked = String::from_utf8(first_frag.into_vec())?;
                    let first_msg = Message::try_from(TcpPakket::new_pakked(&first_pakked)?)?;
                    first_msg
                };
                messages.push(first_msg);

                if other_frags.is_empty() {
                    continue 'process_connection;
                }

                // process central fragments
                let (last_frag, central_frags) = match fragments.split_last() {
                    Some(smt) => (*smt.0, smt.1),
                    None => {
                        continue 'process_connection
                    },
                };

                let _ = central_frags.iter().map(|&msg_bytes| {
                    let pakked = String::from_utf8(msg_bytes.into_vec()?)?;
                    let pakket = TcpPakket::new_pakked(&pakked)?;
                    messages.push(Message::try_from(pakket)?);
                });

                // process last fragment
                match last_frag.last() {
                    Some(&byte) if byte == super::end_delimiter => {
                        let pakked = String::from_utf8(last_frag.to_vec()?)?;
                        let pakket = TcpPakket::new_pakked(&pakked)?;
                        messages.push(Message::try_from(pakket)?);
                    },
                    Some(&byte) => (),
                    None => {
                        // todo: handle this better
                        let err_msg = "The last_frag is empty even though before was found to have a non-null amount of bytes!".to_string();
                        return Err(GenericError::from(err_msg));
                    }
                }

                previous_fragment.append(&mut last_frag.to_vec());
            }
            Ok(_zero) => break 'process_connection,
            Err(_e) => {
                let error_msg = "## The TCP-reader left us. RIP.".to_string();
                return Err(GenericError::from(error_msg));
            }
        }
    }
    Ok(())
}

impl fmt::Display for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] :> {}", self.username, self.content)
    }
}

/// Dispatch is the type of packet unit used excllusively by the server. It associates a message
/// with an identifier, so that the client_handlers can avoid to print messages written by their
/// own clients even in cases of multiple equal nicknames.
#[derive(Debug, Clone)]
pub struct Dispatch {
    userid: usize,
    msg: Message,
}

impl Dispatch {
    pub fn new(userid: usize, msg: Message) -> Dispatch {
        Dispatch { userid, msg }
    }

    pub fn into_msg(self) -> Message {
        self.msg
    }

    pub fn get_userid(&self) -> usize {
        self.userid
    }

    pub fn craft_status_change_dispatch(
        userid: usize,
        username: &str,
        user_status: UserStatus,
    ) -> Dispatch {
        Dispatch {
            userid,
            msg: Message::craft_status_change_msg(username, user_status),
        }
    }
}

impl fmt::Display for Dispatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[#{}] <{},{}>",
            self.userid, self.msg.username, self.msg.content
        )
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn paket() {
        let msg1 = Message::new("carlo", "farabutto!").unwrap();
        assert_eq!(
            TcpPakket::new(r#"|{"username":"carlo","content":"farabutto!"}|"#).unwrap(),
            TcpPakket::from(msg1)
        );
    }

    #[test]
    fn delimiter_validity() {
        assert!(TcpPakket::DELIMITER.is_ascii());
    }

    #[test]
    fn splitting_techniques() {
        let sample: Vec<u8> = b"|arw||1|13nqwr|f089asdfn||qw94|w0oe||09w84ehg||".into_vec();

        let frags: Vec<_> = sample
            .split(|&elem| elem == TcpPakket::DELIMITER as u8)
            //.filter(|&slice| slice.len()!=0)
            .map(|slice| String::from_utf8(slice.into_vec()))
            .collect();

        println! {"{:?}", frags};
    }
}

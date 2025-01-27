use serde::{Deserialize, Serialize};
use std::fmt;
use std::string::ToString;
use std::sync::Arc;

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

pub const INCOMING_BUFFER_LEN: usize = Message::MAX_USERNAME_LEN + Message::MAX_CONTENT_LEN + 20;


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
    pub const MAX_USERNAME_LEN: usize = 30*4;

    /// Maximal length (in bytes) of the content of the message.
    pub const MAX_CONTENT_LEN: usize = 256*4;

    pub fn new(username: &str, content: &str) -> Result<Message> {
        if username.as_bytes().len() > Message::MAX_USERNAME_LEN {
            let error_message = format!("The username '{username}' is too long, it must be at most {} bytes long!", Message::MAX_USERNAME_LEN);
            return Err(GenericError::from(error_message));
        }

        if content.as_bytes().len() > Message::MAX_CONTENT_LEN {
            let error_message = format!("The content is too long, it must be at most {} bytes long!", Message::MAX_CONTENT_LEN);
            return Err(GenericError::from(error_message));
        }

        if content.contains(TcpPaket::DELIMITER) {
            let error_message = "The symbol '|' is not allowed in the chat!".to_string();
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

impl From<TcpPaket> for Message {
    fn from(value: TcpPaket) -> Self {
        let serialized = value.0[1..(value.0.len()-1)].to_string();
        let deserialized_msg = serde_json::from_str::<Message>(&serialized).unwrap();
        deserialized_msg
    }
}


/// TcpPaket is the serialized and delimited Message (thus a string) passed through tcp channels.
#[derive(Debug, PartialEq, Clone)]
pub struct TcpPaket(String);

impl TcpPaket {
    /// Such delimiter is a single char marking the beginning and end of TcpPakets. Notice that it
    /// is fundamental for it to be an ascii value (u8), so that in tcp buffers it is not split.
    const DELIMITER: char = '|';

    pub fn new(candidate: &str) -> Result<TcpPaket> {
        assert!(TcpPaket::DELIMITER.is_ascii());

        let length = candidate.len();

        match (candidate.chars().nth(0_usize), candidate.chars().nth(length-1)) {
            (Some(TcpPaket::DELIMITER), Some(TcpPaket::DELIMITER)) => (),
            (Some(a), Some(b))=> {
                let error_msg = format!("This is not a valid 'paket': the first char is {a} and the last char is {b}! They should be both a '|'");
                return Err(GenericError::from(error_msg))
            },
            _ => return Err(GenericError::from("The candidate is empty!".to_string())),
        }

        let serialized = candidate[1..(length-1)].to_string();
        let deserialized_msg_maybe = serde_json::from_str::<Message>(&serialized);

        match deserialized_msg_maybe {
            Ok(_) => Ok(TcpPaket(candidate.to_string())),
            Err(serde_err) => Err(GenericError::from(serde_err)),
        }
    }
}

impl From<Message> for TcpPaket {
    fn from(msg: Message) -> TcpPaket {
        TcpPaket::new(&format!("|{}|", msg.serialized().unwrap())).unwrap()
    }
}


pub async fn process_msg_wave<Rd>(mut reader: Rd, sender: sync::mpsc::Sender<Message>, mut incoming_buffer: Vec<u8>) -> Result<()>
    where Rd: AsyncRead + Unpin
{
    let previous_fragments = String::with_capacity(INCOMING_BUFFER_LEN);

    'process_wave: loop {
        match reader.read_buf(&mut incoming_buffer).await {
            Ok(n) if n>0 => {

                'process_single_message: loop {
                    let mut message = format!("{previous_fragments}");

                }




            },
            Ok(_zero) => return Ok(()),
            Err(_e) => {
                let error_msg = "## The TCP-reader left us. RIP.".to_string();
                return Err(GenericError::from(error_msg));
            }
        };
    }


    Ok(None)
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
        assert_eq!(TcpPaket::new(r#"|{"username":"carlo","content":"farabutto!"}|"#).unwrap(), TcpPaket::from(msg1));
    }

    #[test]
    fn delimiter_validity() {
        assert!(TcpPaket::DELIMITER.is_ascii());
    }
}
use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::Arc;

pub type GenericError = Box<dyn std::error::Error + Send + Sync + 'static>;
pub type Result<T> = std::result::Result<T, GenericError>;

pub static SERVER_ADDR: &str = "127.0.0.1:6440";
pub const MAX_NUM_USERS: usize = 2000;

pub const MAX_USERNAME_LEN: usize = 40;
pub const MAX_CONTENT_UNIT_LEN: usize = 256;
pub const BUFFER_LEN: usize = MAX_USERNAME_LEN + MAX_CONTENT_UNIT_LEN + 20;

pub type SharedIdPool = Arc<std::sync::Mutex<Vec<usize>>>;

pub fn new_sharded_id_pool() -> SharedIdPool {
    assert!(MAX_NUM_USERS < usize::MAX);

    let id_pool = (1..=MAX_NUM_USERS).rev().collect();
    Arc::new(std::sync::Mutex::new(id_pool))
}

pub fn obtain_id_token(ids: &SharedIdPool) -> Result<usize> {
    let lock_result = ids.lock();
    let mut locked_vec = match lock_result {
        Ok(t) => t,
        Err(_) => return Err(GenericError::from("The mutex of the id pool was poisoned.")),
    };
    locked_vec
        .pop()
        .ok_or(GenericError::from("No ids left in the id pool."))
}

pub fn redeem_token(ids: SharedIdPool, id: usize) -> Result<()> {
    let lock_result = ids.lock();
    let mut locked_vec = match lock_result {
        Ok(t) => t,
        Err(_) => return Err(GenericError::from("The mutex of the id pool was poisoned.")),
    };
    locked_vec.push(id);
    Ok(())
}

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
    pub fn new(username: &str, content: &str) -> Message {
        Message {
            username: username.to_string(),
            content: content.to_string(),
        }
    }

    pub fn get_username(&self) -> String {
        self.username.clone()
    }

    pub fn get_content(&self) -> String {
        self.content.clone()
    }

    pub fn get_content_length(&self) -> usize {
        self.content.len()
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

impl fmt::Display for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] :> {}", self.username, self.content)
    }
}

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

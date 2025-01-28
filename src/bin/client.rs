use std::io::Write;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadHalf, WriteHalf},
    net::TcpStream,
    signal, sync,
};

use chat_server_tokio::*;
use serde_json;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

fn main() -> Result<()> {
    run_client()
}

// ########################################################################################

fn run_client() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    runtime.block_on(async {
        println!("========================");
        println!("Welcome to the KabanChat!");
        println!(
            "WARNING: this chat is still in its alfa version, it is therefore absolutely unsecure, meaning that all that you write will be potentially readable by third parties."
        );
        println!("========================");
        println!();

        let (tcp_rd, tcp_wr, name) = connect_and_login().await?;

        println!(
            r##"Please, {name}, write your messages and press "Enter" to send them. Press CTRL+C to quit the chat."##
        );

        let main_tracker = TaskTracker::new();

        let (shutdown_send, shutdown_recv) = sync::mpsc::channel::<bool>(3);
        let shutdown_token = CancellationToken::new();

        let shutdown_send_wr = shutdown_send.clone();
        let shutdown_token_wr = shutdown_token.clone();

        let shutdown_send_rd = shutdown_send.clone();
        let shutdown_token_rd = shutdown_token.clone();

        let (new_prompt_send, new_prompt_recv) = sync::mpsc::channel::<bool>(3);

        main_tracker.spawn(async move {
            shutdown_signal(shutdown_recv, shutdown_token).await?;
            Ok::<(), GenericError>(())
        });

        main_tracker.spawn(async move {
            wr_manager(tcp_wr, tokio::io::stdin(), &name, shutdown_token_wr, shutdown_send_wr, new_prompt_send).await?;
            Ok::<(), GenericError>(())
        });

        main_tracker.spawn(async move {
            rd_manager(tcp_rd, shutdown_token_rd, shutdown_send_rd, new_prompt_recv).await?;
            Ok::<(), GenericError>(())
        });

        main_tracker.close();
        main_tracker.wait().await;

        println!(r##"All tasks are terminated, the shutdown process is completed!"##);
        Ok::<(), GenericError>(())
    })?;

    runtime.shutdown_timeout(std::time::Duration::from_secs(0));
    Ok(())
}

// ########################################################################################

/// We ask here for the nickname of the user, we connect to the server, and we greet him ("helo" message)
async fn connect_and_login() -> Result<(ReadHalf<TcpStream>, WriteHalf<TcpStream>, String)> {
    let name = loop {
        print!(r###"Please input your desired userid and press "Enter": "###);
        std::io::stdout().flush()?;

        let mut name = String::new();
        std::io::stdin().read_line(&mut name)?;
        let name = name.trim().to_string();

        if name.as_bytes().len() < Message::MAX_USERNAME_LEN {
            break name;
        } else {
            println!(
                r###"This username is too long! It must not be longer than {} chars!"###,
                Message::MAX_USERNAME_LEN
            );
        }
    };

    let stream = TcpStream::connect(SERVER_ADDR).await?;
    let (tcp_rd, mut tcp_wr) = tokio::io::split(stream);

    let serialized_helo_msg = serde_json::to_string(&Message::new(&name, "helo")?)?;

    tcp_wr.write_all(serialized_helo_msg.as_bytes()).await?;
    tcp_wr.flush().await?;

    Ok((tcp_rd, tcp_wr, name.to_string()))
}

async fn shutdown_signal(
    mut shutdown_recv: sync::mpsc::Receiver<bool>,
    shutdown_token: CancellationToken,
) -> Result<()> {
    tokio::select! {
        _ = signal::ctrl_c() => {},
        _ = shutdown_recv.recv() => {},
    }

    shutdown_token.cancel();

    println!();
    println!("========================");
    println!(r##"Starting the shutdown process"##);

    Ok(())
}

// todo: adapt this manager to split the written messages in chunks of maximum `Max_content_len` to
// write the messages in the writer
async fn wr_manager<Wr, Rd>(
    mut tcp_wr: Wr,
    mut stdin: Rd,
    name: &str,
    shutdown_token: CancellationToken,
    shutdown_send: sync::mpsc::Sender<bool>,
    new_prompt_send: sync::mpsc::Sender<bool>,
) -> Result<()>
where
    Wr: AsyncWrite + Unpin,
    Rd: AsyncRead + Unpin,
{
    let mut writing_buffer: Vec<u8> = Vec::with_capacity(Message::MAX_CONTENT_LEN);

    'prompt: loop {
        print!("\n{name}:> ");
        std::io::stdout().flush()?;

        loop {
            writing_buffer.clear();

            tokio::select! {
                _ = shutdown_token.cancelled() => break 'prompt,

                result = stdin.read_buf(&mut writing_buffer) => {

                    match result {
                        Ok(n) if n>0 => {
                            let is_last_chunk = if writing_buffer.last() == Some(&b'\n') {
                                writing_buffer.pop();
                                true
                            } else {
                                false
                            };

                            if writing_buffer.len() > 0 {
                                let text = String::from_utf8(writing_buffer.clone())?;

                                let msg = match Message::new(&name, &text) {
                                    Ok(msg) => msg,
                                    Err(e) => {
                                        println!("The message could not be processed because of the subsequent error: {e}\nPlease adapt you input accordingly.");
                                        continue 'prompt;
                                    }
                                };
                                let serialized = msg.serialized()?;
                                let paketed = format!("| {serialized} |");

                                tcp_wr.write_all(paketed.as_bytes()).await?;
                                new_prompt_send.send(true).await?;
                            }

                            if is_last_chunk {
                                continue 'prompt;
                            }
                        },
                        Ok(_) | _ => {result?;}
                    }
                }
            }
        }
    }

    println!(r##"Terminating the writing task"##);

    shutdown_send.send(true).await?;

    Ok::<(), GenericError>(())
}

async fn rd_manager<Rd>(
    mut tcp_rd: Rd,
    shutdown_token: CancellationToken,
    shutdown_send: sync::mpsc::Sender<bool>,
    mut new_prompt_recv: sync::mpsc::Receiver<bool>,
) -> Result<()>
where
    Rd: AsyncRead + Unpin,
{
    let mut incoming_buffer: Vec<u8> = Vec::with_capacity(INCOMING_BUFFER_LEN);

    // todo: check to write the write '\n' after the first prompt appears
    // for now this will be raw by adding '\n' every time

    loop {
        incoming_buffer.clear();

        // todo: framing must be corrected here as well. The best would be to address this in the library

        tokio::select! {
            _ = shutdown_token.cancelled() => break,
            prompt_result = new_prompt_recv.recv() => {
                match prompt_result {
                    Some(_) => println!("\n"),
                    None => return Err(GenericError::from("No new prompt transmitted".to_string())),
                }
            },
            size_result = tcp_rd.read_buf(&mut incoming_buffer) => {
                match size_result? {
                    0 => break,
                    _ => {
                        let incoming_msg = {
                            let serialized_msg = String::from_utf8(incoming_buffer.clone())?;
                            serde_json::from_str::<Message>(&serialized_msg)?
                        };

                        println!("\n{incoming_msg}");
                    },
                }
            },
        }
    }

    println!(r##"Terminating the reading task"##);

    shutdown_send.send(true).await?;

    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;
    use std::sync::mpsc::channel;

    #[tokio::test]
    async fn writing_frames() -> Result<()> {
        use tokio::io::BufReader;

        let username = "pino";

        let msg1: String = std::iter::repeat("a").take(200).collect();
        let msg2: String = std::iter::repeat("b").take(250).collect();
        let msg3: String = std::iter::repeat("c").take(270).collect();

        let protoreader = tokio_test::io::Builder::new()
            .read(msg1.as_bytes())
            .read(msg2.as_bytes())
            .read(msg3.as_bytes())
            .build();
        let mock_stdin = BufReader::new(protoreader);

        let stdout = tokio::io::stdout();

        let (shutdown_tx, _receiver) = sync::mpsc::channel::<bool>(1);

        let shutdown_token = CancellationToken::new();

        let (sender, _receiver) = sync::mpsc::channel::<bool>(7);

        wr_manager(
            stdout,
            mock_stdin,
            username,
            shutdown_token,
            shutdown_tx,
            sender,
        )
        .await?;

        Ok::<(), GenericError>(())
    }
}

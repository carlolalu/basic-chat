use std::io::Write;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadHalf, WriteHalf},
    net::TcpStream,
    signal, sync,
};

use chat_server_tokio::paket::*;
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

        main_tracker.spawn(async move {
            shutdown_signal(shutdown_recv, shutdown_token).await?;
            Ok::<(), GenericError>(())
        });

        main_tracker.spawn(async move {
            wr_manager(tcp_wr, tokio::io::stdin(), &name, shutdown_token_wr, shutdown_send_wr).await?;
            Ok::<(), GenericError>(())
        });

        main_tracker.spawn(async move {
            rd_manager(tcp_rd, shutdown_token_rd, shutdown_send_rd).await?;
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

        if name.len() < MAX_USERNAME_LEN {
            break name;
        } else {
            println!(r###"This username is too long!"###);
        }
    };

    let stream = TcpStream::connect(SERVER_ADDR).await?;
    let (tcp_rd, mut tcp_wr) = tokio::io::split(stream);

    let serialised_helo_msg = serde_json::to_string(&Message::new(&name, "helo"))?;

    tcp_wr.write_all(serialised_helo_msg.as_bytes()).await?;
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

async fn wr_manager<Wr, Rd>(
    mut tcp_wr: Wr,
    mut stdin: Rd,
    name: &str,
    shutdown_token: CancellationToken,
    shutdown_send: sync::mpsc::Sender<bool>,
) -> Result<()>
where
    Wr: AsyncWrite + Unpin,
    Rd: AsyncRead + Unpin,
{
    let mut outgoing_buffer: Vec<u8> = Vec::with_capacity(MAX_CONTENT_UNIT_LEN);

    'prompt: loop {
        print!("\n{name}:> ");
        std::io::stdout().flush()?;

        loop {
            outgoing_buffer.clear();

            tokio::select! {
                _ = shutdown_token.cancelled() => break 'prompt,

                result = stdin.read_buf(&mut outgoing_buffer) => {

                    match result {
                        Ok(n) if n>0 => {
                            let is_last_chunk = if outgoing_buffer.last() == Some(&b'\n') {
                                outgoing_buffer.pop();
                                true
                            } else {
                                false
                            };

                            if outgoing_buffer.len() > 0 {
                                let text = String::from_utf8(outgoing_buffer.clone())?;
                                let serialized = Message::new(&name, &text).serialized()?;

                                tcp_wr.write_all(serialized.as_bytes()).await?;
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
) -> Result<()>
where
    Rd: AsyncRead + Unpin,
{
    let mut incoming_buffer: Vec<u8> = Vec::with_capacity(BUFFER_LEN);

    // todo: check to write the write '\n' after the first prompt appears
    // for now this will be raw by adding '\n' every time

    loop {
        incoming_buffer.clear();

        tokio::select! {
            _ = shutdown_token.cancelled() => break,
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
            }
        }
    }

    println!(r##"Terminating the reading task"##);

    shutdown_send.send(true).await?;

    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;

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

        wr_manager(stdout, mock_stdin, username, shutdown_token, shutdown_tx).await?;

        Ok::<(), GenericError>(())
    }

}

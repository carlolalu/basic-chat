use std::io::Write;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadHalf, WriteHalf},
    net::TcpStream,
    signal, sync,
};

use chat_server_tokio::paket::{GenericError, Message, Result, SERVER_ADDR};
use serde_json;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

fn main() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    runtime.block_on(
        async {
            println!("========================");
            println!("Welcome to the KabanChat!");
            println!(
                "WARNING: this chat is still in its alfa version, it is therefore absolutely unsecure, meaning that all that you write will be potentially readable by third parties."
            );
            println!("========================");
            println!();

            let (tcp_rd, tcp_wr, name) = connect_and_login().await?;

            println!(
                r##"Please, {name}, write your messages and press "Enter" to send them. Press CTRL+C or send the message "EXIT" to quit the connection."##
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
                wr_manager(tcp_wr, &name, shutdown_token_wr, shutdown_send_wr).await?;
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

async fn connect_and_login() -> Result<(ReadHalf<TcpStream>, WriteHalf<TcpStream>, String)> {
    print!(r###"Please input your desired userid and press "Enter": "###);
    std::io::stdout().flush()?;

    let mut name = String::new();
    std::io::stdin().read_line(&mut name)?;
    let name = name.trim().to_string();

    let stream = TcpStream::connect(SERVER_ADDR).await?;
    let (tcp_rd, mut tcp_wr) = tokio::io::split(stream);

    // send "helo" message
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

async fn wr_manager<Wr>(
    mut tcp_wr: Wr,
    name: &str,
    shutdown_token: CancellationToken,
    shutdown_send: sync::mpsc::Sender<bool>,
) -> Result<()>
where
    Wr: AsyncWrite + Unpin,
{
    let mut outgoing_buffer: Vec<u8> = Vec::with_capacity(256);

    let mut stdin = tokio::io::stdin();

    loop {
        outgoing_buffer.clear();

        print!("\n{name}:> ");
        std::io::stdout().flush()?;

        tokio::select! {
            _ = shutdown_token.cancelled() => break,
            result = stdin.read_buf(&mut outgoing_buffer) => {
                result?;

                // remove the last '\n'
                outgoing_buffer.pop();

                if outgoing_buffer.len() > 0 {
                    let text = String::from_utf8(outgoing_buffer.clone())?;

                    if text == "EXIT".to_string() {
                        break;
                    }

                    let outgoing_msg = Message::new(&name, &text);
                    let serialised = serde_json::to_string(&outgoing_msg)?;

                    tcp_wr.write_all(serialised.as_bytes()).await?;
                }
            }

        }
    }
    println!(r##"Closing the writing task"##);

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
    let mut incoming_buffer: Vec<u8> = Vec::with_capacity(256);

    loop {
        incoming_buffer.clear();

        tokio::select! {
            _ = shutdown_token.cancelled() => break,
            size_result = tcp_rd.read_buf(&mut incoming_buffer) => {
                match size_result? {
                    0 => break,
                    _ => {
                        let incoming_msg = Message::from_serialized_buffer(&incoming_buffer)?;
                        println!("{incoming_msg}");
                    },
                }
            }
        }
    }

    println!(r##"Closing the reading task"##);

    shutdown_send.send(true).await?;

    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::client::start_client;

    #[tokio::test]
    async fn buff_n_framing(num_clients: usize) -> Result<()> {
        todo!();
    }

    #[tokio::test]
    async fn many_client(num_clients: usize) -> Result<()> {
        todo!();
    }
}

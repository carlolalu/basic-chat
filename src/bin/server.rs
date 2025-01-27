use chat_server_tokio::*;
use std::sync::Arc;
use tokio::{
    io::{self, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    signal, sync,
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

#[tokio::main]
async fn main() -> Result<()> {
    println!("Welcome, the KabanChat server receives at maximum {MAX_NUM_USERS} users at the address {SERVER_ADDR}");

    let (client_handler_tx, dispatcher_rx) = sync::mpsc::channel::<Dispatch>(50);
    let (dispatcher_tx, _rx) = sync::broadcast::channel::<Dispatch>(50);

    let dispatcher_tx_arc = Arc::new(dispatcher_tx);
    let dispatcher_mic = dispatcher_tx_arc.clone();

    let shutdown_token = CancellationToken::new();
    let shutdown_token_manager = shutdown_token.clone();

    let shutdown_token_dispatcher = CancellationToken::new();
    let shutdown_token_dispatcher_handle = shutdown_token_dispatcher.clone();

    let (shutdown_tx, shutdown_recv) = sync::mpsc::channel::<bool>(2);
    let _shutdown_tx_manager = shutdown_tx.clone();
    let _shutdown_tx_dispatcher = shutdown_tx.clone();

    let main_tracker = TaskTracker::new();

    main_tracker.spawn(async move {
        shutdown_signal(shutdown_recv, shutdown_token).await?;
        Ok::<(), GenericError>(())
    });

    main_tracker.spawn(async move {
        dispatcher(dispatcher_rx, dispatcher_mic, shutdown_token_dispatcher).await?;
        Ok::<(), GenericError>(())
    });

    main_tracker.spawn(async move {
        server_manager(dispatcher_tx_arc, client_handler_tx, shutdown_token_manager, shutdown_token_dispatcher_handle).await?;
        Ok::<(), GenericError>(())
    });

    main_tracker.close();
    main_tracker.wait().await;

    println!("The server is shutting down!");

    Ok(())
}

// ########################################################################################

async fn shutdown_signal(
    mut shutdown_recv: sync::mpsc::Receiver<bool>,
    shutdown_token: CancellationToken,
) -> Result<()> {
    tokio::select! {
        _ = signal::ctrl_c() => {},
        _ = shutdown_recv.recv() => {},
    }

    shutdown_token.cancel();

    println!("General shutdown signal given");

    Ok(())
}

/// This function is responsible for passing dispatches between client_handlers
async fn dispatcher(
    mut dispatcher_rx: sync::mpsc::Receiver<Dispatch>,
    dispatcher_mic: Arc<sync::broadcast::Sender<Dispatch>>,
    shutdown_token_dispatcher: CancellationToken,
) -> Result<()> {
    let task_id = "# [Dispatcher]";

    println!("{task_id}:> Starting ...");
    loop {
        tokio::select! {
            _ = shutdown_token_dispatcher.cancelled() => break,

            transmission = dispatcher_rx.recv() => {
                if let Some(dispatch) = transmission {
                    dispatcher_mic.send(dispatch)?;
                }
            }
        }
    }

    println!("{task_id}:> The dispatcher is shutting down.");

    Ok::<(), GenericError>(())
}

/// This function creates a socket and accepts connections on it, spawning for each of them a new task handling it.
async fn server_manager(
    dispatcher_tx_arc: Arc<sync::broadcast::Sender<Dispatch>>,
    client_handler_tx: sync::mpsc::Sender<Dispatch>,
    shutdown_token: CancellationToken,
    shutdown_token_dispatcher_handle : CancellationToken
) -> Result<()> {

    let task_id = "# [Server Manager]";

    println!("{task_id}:> Starting.");

    let listener = TcpListener::bind(SERVER_ADDR).await?;

    let server_manager_tracker = TaskTracker::new();

    let id_pool = SharedIdPool::new();

    loop {
        if server_manager_tracker.len() > MAX_NUM_USERS {
            println!("{task_id}:> A new connection was requested but the number of connected clients has reached the maximum.");
        }

        let shutdown_token_client = shutdown_token.clone();

        tokio::select! {
            _ = shutdown_token.cancelled() => {
                println!("{task_id}:> Stop accepting new connections, waiting for the current ones to be interrupted.");
                break
            },
            result = listener.accept() => {
                let (stream, addr) = result?;
                let client_handler_tx = client_handler_tx.clone();
                let dispatcher_subscriber = dispatcher_tx_arc.clone();

                let client_handler_rx = dispatcher_subscriber.subscribe();

                let id_pool_local_arc = id_pool.clone();

                server_manager_tracker.spawn(async move {
                    client_handler(
                        stream,
                        client_handler_tx,
                        client_handler_rx,
                        addr,
                        id_pool_local_arc,
                        shutdown_token_client,
                    ).await?;
                    Ok::<(), GenericError>(())
                });
            },
        }
    }

    server_manager_tracker.close();
    server_manager_tracker.wait().await;

    println!("{task_id}:> No connection active anymore, waiting for the dispatcher to shutdown.");

    shutdown_token_dispatcher_handle.cancel();

    println!("{task_id}:> The server manager is shutting down.");

    Ok::<(), GenericError>(())
}

/// The client handler divides the stream into reader and writer, and then spawns two tasks handling them.
async fn client_handler(
    stream: TcpStream,
    client_handler_tx: sync::mpsc::Sender<Dispatch>,
    client_handler_rx: sync::broadcast::Receiver<Dispatch>,
    addr: std::net::SocketAddr,
    id_pool: SharedIdPool,
    shutdown_token: CancellationToken,
) -> Result<()> {
    let userid = id_pool.obtain_id_token()?;
    let task_id = format!("@ Handler of ['{userid}':{addr}]");
    let client_handler_task_manager = TaskTracker::new();

    println!("{task_id}:> Starting...");
    let task_id_handle1 = Arc::new(task_id.clone());
    let task_id_handle2 = task_id_handle1.clone();

    let (tcp_rd, tcp_wr) = io::split(stream);

    let client_token = CancellationToken::new();

    let client_token_wr = client_token.clone();
    let client_token_rd = client_token.clone();

    client_handler_task_manager.spawn(async move {
        match client_tcp_wr_loop(
            tcp_wr,
            &task_id_handle1,
            client_handler_rx,
            userid,
            &client_token_wr,
        )
        .await {
            Ok(()) => (),
            Err(e) => eprintln!("{task_id_handle1} The writer crashed: '{e}'"),
        }
        client_token_wr.cancel();
    });

    client_handler_task_manager.spawn(async move {
        match client_tcp_rd_loop(
            tcp_rd,
            &task_id_handle2,
            client_handler_tx,
            userid,
            &client_token_rd,
        )
        .await {
            Ok(()) => (),
            Err(e) => eprintln!("{task_id_handle2} The reader crashed: '{e}'"),
        }
        client_token_rd.cancel();
    });

    tokio::select! {
        _ = shutdown_token.cancelled() => client_token.cancel(),
        _ = client_token.cancelled() => (),
    }

    client_handler_task_manager.close();
    client_handler_task_manager.wait().await;

    id_pool.redeem_token(userid)?;
    println!("{task_id}:> Id token redeemed.");

    println!("{task_id}:> Terminating task.");

    Ok(())
}

async fn client_tcp_wr_loop(
    mut tcp_wr: (impl AsyncWrite + Unpin),
    supertask_id: &Arc<String>,
    mut client_handler_rx: sync::broadcast::Receiver<Dispatch>,
    userid: usize,
    client_token: &CancellationToken,
) -> Result<()> {
    let task_id = format!("{supertask_id} (TCP-writer)");

    loop {
        tokio::select! {
            _ = client_token.cancelled() => break,
            result = client_handler_rx.recv() => {
                match result {
                    Ok(dispatch) => {
                        if dispatch.get_userid() != userid {
                            println!("{task_id}:> Dispatch received.");

                            let serialised_msg = serde_json::to_string(&dispatch.into_msg())?;
                            tcp_wr.write_all(serialised_msg.as_bytes()).await?;

                            println!("{task_id}:> The msg wrapped in the dispatch was sent.");
                        }
                    }
                    Err(e) => return Err::<(), GenericError>(GenericError::from(e)),
                }
            }
        }
    }

    let serialised_msg = serde_json::to_string(&Message::craft_server_interrupt_msg())?;
    tcp_wr.write_all(serialised_msg.as_bytes()).await?;

    println!("{task_id}:> Terminating task.");

    Ok(())
}

async fn client_tcp_rd_loop(
    mut tcp_rd: impl AsyncRead + Unpin,
    supertask_id: &Arc<String>,
    client_handler_tx: sync::mpsc::Sender<Dispatch>,
    userid: usize,
    client_token: &CancellationToken,
) -> Result<()> {
    let task_id = format!("{supertask_id} (TCP-recv)");

    let mut buffer_incoming = Vec::with_capacity(INCOMING_BUFFER_LEN);

    let username =
        obtain_username(&task_id, &mut tcp_rd, &mut buffer_incoming, client_token).await?;

    let init_dispatch = Dispatch::new(
        userid,
        Message::craft_status_change_msg(&username, UserStatus::Present),
    );

    client_handler_tx.send(init_dispatch).await?;

    println!(r##"{task_id}:> Handling {username}"##);

    loop {
        buffer_incoming.clear();

        tokio::select! {
            _ = client_token.cancelled() => break,

            transmission = tcp_rd.read_buf(&mut buffer_incoming) => {
                match transmission {

                    // todo: here manage chunks which are longer than BUFFER_DIMENSION chars
                    // how to change this? How to act for such situations?
                    Ok(n) if n > 0 => {
                        let message = {

                            // todo: first isolate the "paketed" message, constrained by the symbols "|".

                            let pakets_n_fragment = String::from_utf8(buffer_incoming.clone())?;


                            // * with the 1st message:
                            // 1. divide paket_fragments in various pakets + 1 fragment
                            // 2. serde_json on each paket and sew together their content
                            // 3. append the last fragment to the fragments_buffer

                            // * with subsequent messages:
                            // 1. find the first "|" and if it is in the beginning then start the procedure above
                            //        else read till "|", attach this first part to the fragments buffer above and then repeat the procedure above
                            serde_json::from_str::<Message>(&pakets_n_fragment)
                        };

                        // debug
                        println!(r###"The message is ####{:?}####"###, message);

                        let message = message?;

                        println!("{task_id}:> Message of content-length {} (in chars) received.", message.get_content().len());

                        let dispatch = Dispatch::new(userid, message);
                        client_handler_tx.send(dispatch.clone()).await?;

                        println!("{task_id}:> Message wrapped in a dispatch and sent.")
                    },
                    Ok(_zero) => {
                        let final_dispatch = Dispatch::craft_status_change_dispatch(
                            userid,
                            &username,
                            UserStatus::Absent,
                        );
                        client_handler_tx.send(final_dispatch.clone()).await?;
                        println!("{task_id}:> The connection is no longer active. Final dispatch sent.");
                        break;
                    },
                    Err(_e) => {
                        println!("## The TCP-reader left us. RIP.");
                        break;
                    }
                };
            }
        }
    }

    println!("{task_id}:> Terminating task.");

    Ok(())
}

async fn obtain_username(
    task_id: &str,
    tcp_rd: &mut (impl AsyncRead + Unpin),
    buffer_incoming: &mut Vec<u8>,
    client_token: &CancellationToken,
) -> Result<String> {
    tokio::select! {
        _ = client_token.cancelled() => {
            let err_msg = format!("{task_id}:> The shutdown process started.");
            let e = GenericError::from(io::Error::new(io::ErrorKind::NotFound, err_msg));
            Err(e)
        },

        transmission = tcp_rd.read_buf(buffer_incoming) => {

            let err = {
                let err_msg = format!("{task_id}:> The 'helo' message was not received");
                GenericError::from(io::Error::new(io::ErrorKind::NotFound, err_msg))
            };

            match transmission {
                Ok(n) if n > 0 => {
                    let incoming_msg = {
                        let serialized_msg = String::from_utf8(buffer_incoming.clone())?;
                        serde_json::from_str::<Message>(&serialized_msg)?
                    };

                    if &incoming_msg.get_content() == "helo" {
                        Ok(incoming_msg.get_username())
                    } else {
                        Err(err)
                    }
                },
                Ok(_) | _ => Err(err),
            }
        },
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[tokio::test]
    async fn reading_frames() -> Result<()> {
        use tokio::io::BufReader;

        let username = "pino";
        let content1: String = std::iter::repeat("a").take(200).collect();
        let content2: String = std::iter::repeat("a").take(300).collect();

        let serialized_helo = Message::new(username, "helo")?.serialized()?;
        let serialized_msg1 = Message::new(username, &content1)?.serialized()?;
        let serialized_msg2 = Message::new(username, &content2)?.serialized()?;
        let reader = tokio_test::io::Builder::new()
            .read(serialized_helo.as_bytes())
            .read(serialized_msg1.as_bytes())
            .read(serialized_msg2.as_bytes())
            .build();
        let reader = BufReader::new(reader);

        let supertask_id = &Arc::new("supertask_id".to_string());

        let (client_handler_tx, mut receiver) = sync::mpsc::channel::<Dispatch>(50);

        let userid = 1;

        let client_token = &CancellationToken::new();

        client_tcp_rd_loop(reader, supertask_id, client_handler_tx, userid, client_token).await?;

        while let Some(received) = receiver.recv().await {
            println!("The dispatch received is: {received}");
        }

        Ok::<(), GenericError>(())
    }
}

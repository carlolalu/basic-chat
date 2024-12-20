use chat_server_tokio::paket::*;
use std::sync::Arc;
use tokio::{
    io::{self, AsyncReadExt, AsyncRead, AsyncWrite, AsyncWriteExt},
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
    let shutdown_token_dispatcher = shutdown_token.clone();

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
        server_manager(dispatcher_tx_arc, client_handler_tx, shutdown_token_manager).await?;
        Ok::<(), GenericError>(())
    });

    main_tracker.close();
    main_tracker.wait().await;

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

    Ok(())
}

/// This function is responsible for passing dispatches between client_handlers
async fn dispatcher(
    mut dispatcher_rx: sync::mpsc::Receiver<Dispatch>,
    dispatcher_mic: Arc<sync::broadcast::Sender<Dispatch>>,
    shutdown_token: CancellationToken,
) -> Result<()> {
    let dispatcher_name = "# [Dispatcher]".to_string();

    println!("{dispatcher_name}:> Starting ...");
    loop {
        tokio::select! {
            // no final dispatch: it is created locally on each client handler tcp writer
            _ = shutdown_token.cancelled() => break,

            transmission = dispatcher_rx.recv() => {
                if let Some(dispatch) = transmission {
                    dispatcher_mic.send(dispatch)?;
                }
            }
        }
    }

    Ok::<(), GenericError>(())
}

/// This function creates a socket and accepts connections on it, spawning for each of them a new task handling it.
async fn server_manager(
    dispatcher_tx_arc: Arc<sync::broadcast::Sender<Dispatch>>,
    client_handler_tx: sync::mpsc::Sender<Dispatch>,
    shutdown_token: CancellationToken,
) -> Result<()> {
    println!("Start server manager loop!");

    let listener = TcpListener::bind(SERVER_ADDR).await?;

    let server_manager_tracker = TaskTracker::new();

    let id_pool = new_sharded_id_pool();

    loop {
        if server_manager_tracker.len() > MAX_NUM_USERS {
            println!("A new connection was requested but the number of connected clients has reached the maximum.");
        }

        let shutdown_token_client = shutdown_token.clone();

        tokio::select! {
            _ = shutdown_token.cancelled() => break,
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

    println!("The server will thus now shut off!");

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
    let userid = obtain_id_token(&id_pool)?;
    let task_id = format!("@ Handler of [{userid}:{addr}]");
    let client_handler_task_manager = TaskTracker::new();

    println!("{task_id}:> Starting...");
    let task_id_handle1 = Arc::new(task_id.clone());
    let task_id_handle2 = task_id_handle1.clone();

    let (tcp_rd, tcp_wr) = io::split(stream);

    let client_token = CancellationToken::new();

    let client_token_wr = client_token.clone();
    let client_token_rd = client_token.clone();

    client_handler_task_manager.spawn(async move {
        client_tcp_wr_loop(
            tcp_wr,
            task_id_handle1,
            client_handler_rx,
            userid,
            client_token_wr,
        )
        .await?;
        Ok::<(), GenericError>(())
    });

    client_handler_task_manager.spawn(async move {
        client_tcp_rd_loop(
            tcp_rd,
            task_id_handle2,
            client_handler_tx,
            userid,
            client_token_rd,
        )
        .await?;
        Ok::<(), GenericError>(())
    });

    shutdown_token.cancelled().await;
    client_token.cancel();

    client_handler_task_manager.close();
    client_handler_task_manager.wait().await;

    redeem_token(id_pool, userid)?;
    println!("{task_id}:> Id token redeemed.");

    println!("{task_id}:> Terminating task.");

    Ok(())
}


async fn client_tcp_wr_loop(
    mut tcp_wr: (impl AsyncWrite + Unpin),
    supertask_id: Arc<String>,
    mut client_handler_rx: sync::broadcast::Receiver<Dispatch>,
    userid: usize,
    client_token: CancellationToken,
) -> Result<()> {
    let task_id = format!("{supertask_id} (TCP-writer)");

    loop {
        tokio::select! {
            _ = client_token.cancelled() => {
                let serialised_msg = serde_json::to_string(&Message::craft_server_shutdown_msg())?;
                tcp_wr.write_all(serialised_msg.as_bytes()).await?;
                break;
            }

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

    client_token.cancel();

    println!("{task_id}:> Terminating task.");

    Ok(())
}

async fn client_tcp_rd_loop(
    mut tcp_rd: impl AsyncRead + Unpin,
    supertask_id: Arc<String>,
    client_handler_tx: sync::mpsc::Sender<Dispatch>,
    userid: usize,
    client_token: CancellationToken,
) -> Result<()> {
    let task_id = format!("{supertask_id} (TCP-recv)");

    let mut buffer_incoming = Vec::with_capacity(256);

    let username =
        obtain_username(&task_id, &mut tcp_rd, &mut buffer_incoming, &client_token).await?;

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
                    Ok(n) => {
                        if n > 0 {
                            println!("{task_id}:> Message received.");

                            let dispatch =
                                Dispatch::new(userid, Message::from_serialized_buffer(&buffer_incoming)?);

                            client_handler_tx.send(dispatch.clone()).await?;

                            println!("{task_id}:> Message wrapped in a dispatch and sent.");
                        } else {
                            // Ok(0) implies one of two possible scenarios, both could be treated separately (not needed here IMHO)
                            // https://docs.rs/tokio/latest/tokio/io/trait.AsyncReadExt.html#method.read_buf
                            let final_dispatch = Dispatch::craft_status_change_dispatch(
                                userid,
                                &username,
                                UserStatus::Absent,
                            );
                            client_handler_tx.send(final_dispatch.clone()).await?;
                            println!("{task_id}:> The connection is no longer active. Final dispatch sent.");
                            break;
                        }
                    }

                    Err(_e) => {
                        println!("## The TCP-reader left us. RIP.");
                        break;
                    }
                };
            }
        }
    }

    client_token.cancel();

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
            match transmission {
                Ok(n) if n > 0 => {
                    let helo_msg = Message::from_serialized_buffer(&buffer_incoming)?;

                    Ok(helo_msg.get_username())
                }

                Ok(_) | _ => {
                    let err_msg = format!("{task_id}:> The 'helo' message was not received");
                    let e = GenericError::from(io::Error::new(io::ErrorKind::NotFound, err_msg));
                    Err(e)
                }
            }
        },
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[tokio::test]
    async fn buff_n_framing(num_clients: usize) -> Result<()> {
        todo!();
    }
}

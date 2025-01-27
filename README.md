# User's Guide

Run the server with `cargo run --bin server`, and the client with `cargo run --bin client`. The client will then guide the chat user in the process.

# Objectives

Implement a chat server (step by-step)

- GUI: separate output and input in the clients + on the server side have a tree showing what is happening inside the server in a structured way.

- basic level for an admin authentication: if one gives a specific commands and provides the right password, it can control the server (only one person per time must be granted such access)

- make the id pool somehow sharded: it will handle better backpressure from many clients.

- I would like to make this library a library capable of creating an ecosystem of servers and clients connected to each others, in order to create big networks. This should be something where from an integration test one could activate a modality in which it uses a single client but also in which it might decide to activate many and use them with an OOP fashion: `client.connect_to_server(addr)`, `client.login(name)`, `client.send_msg(msg)` etc....

## Developer's guide

Here I should draw a graph to explain the simple architecture of my server
```mermaid

```

## my diary

How to test what is happening with framing and buffering? Should I make some small unit test?


### channels and strings

- Concept: the client side sends and receives these structs: `struct Message { username: String, content: String }`, but this structure on the client side is wrapped inside a Dispatch: `struct Dispatch {userid: SomeType, msg : Message }`. This so that I do not have to implement any protocol to let a client find a unique username, but at the same time, the clienthandler can avoid to send to its client the messages sent by himself, and instead filter out  the ones sent by eventual people with the same name (by exploiting the id).

- convert Vec<u8> -> String by `String::from_utf8()?` e String -> Vec by `string.as_bytes()`

- data is NOT cleaned from new line characters `\n`

# Objectives

Implement a chat server (step by-step)

- add a GUI, so that you can separate output and input in the clients. On the server side could be cool to have a tree showing what is happening inside the server in a structured way.

- add a general tests (recall that to see the output you might want to use the arg `--nocapture`) that on different screens executes the server and a number of clients which connect to it and chat among themselves

- implement  basic level for an admin authentication: if one gives a specific commands and provides the right password, it can control the server (only one person per time must be granted such access)

- add some security and cryptography. Just the basics.

## T. hot recommendations

- framing and buffering: what happens when a message is longer than 250 chars? There is a tokio_util instrument about framing and buffering.

## Developer's guide

```mermaid

```

### channels and strings

- Concept: the client side sends and receives these structs: `struct Message { username: String, content: String }`, but this structure on the client side is wrapped inside a Dispatch: `struct Dispatch {userid: SomeType, msg : Message }`. This so that I do not have to implement any protocol to let a client find a unique username, but at the same time, the clienthandler can avoid to send to its client the messages sent by himself, and instead filter out  the ones sent by eventual people with the same name (by exploiting the id).

- convert Vec<u8> -> String by `String::from_utf8()?` e String -> Vec by `string.as_bytes()`

- data is NOT cleaned from new line characters `\n`

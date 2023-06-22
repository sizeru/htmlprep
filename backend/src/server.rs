use std::collections::HashMap;
use std::convert::Infallible;
use std::fs::{File, self};
use std::io::{prelude::*, BufReader, self, ErrorKind};
use std::ops::BitOr;
use std::path::Path;
use std::rc::Rc;
use std::env::{self, current_dir};
use std::net::{SocketAddr, SocketAddrV4};
use std::str;
use std::str::FromStr;
use chrono::{Utc, TimeZone, DateTime};
use chrono_tz::Etc::GMTPlus4;
use compress::zlib;
use crc::Crc;
use hyper::body::Body;
use num_digitize::FromDigits;
use postgres::{Client, NoTls, GenericClient};
use pwhash::sha512_crypt;
use signal_hook::iterator::exfiltrator::raw;
// use crate::{http, config_parser, http::*, get_server_status, server};
use crate::{get_server_status, CONFIG};
// The following use statements were made after the cmdutil refactor
use signal_hook::{consts::*};
use signal_hook_tokio::Signals;
use std::{thread, fmt};
use std::os::unix::net::{UnixStream, UnixListener, UnixDatagram};
use futures_util::StreamExt;
use bytes::{Bytes, BytesMut};
use http_body_util::Full;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use tokio::net::TcpListener;



use std::{process, error, default};
use log::{trace, debug, info, warn, error};
use simplelog;
use chrono;
use serde::{Serialize, Deserialize};
use const_format::formatcp;
use std::os::unix::fs::*;

pub const EXECUTABLE_NAME: &str = "rmc";

static mut status: ServerStatus = ServerStatus { state: ServerState::Unreachable };

/// Runs the server inside of the current process. No daemon. 
pub async fn run() -> Result<(), Box<dyn error::Error>> { 
    {
        unsafe{
            status.state = ServerState::Starting;
        }
        let runtime_dir = Server::runtime_dir()?;
        match fs::metadata(&runtime_dir) { // Used to check if path exists {
            Ok(_) => {
                // TODO: This branch needs work. If there is a valid server 
                // status, that means the server is running somehow. Else, the 
                // server failed to shut down properly
                println!(concat!("Trying to start the server, but the runtime ",
                    "directory already exists. This means that the server is ",
                    "either still running, or did not shut down correctly. ",
                    "Sometimes a call to `rmc repair` can fix this."));
                let server_status = get_server_status().await?;
                return Err(Box::new(Error::ServerAlreadyStarted(server_status)));
            }
            Err(error) => match error.kind() {
                io::ErrorKind::PermissionDenied => {
                    println!("Could not access the server's runtime directory. The server may have been started by another user.");
                    return Err(Box::new(error));
                },
                io::ErrorKind::NotFound => { // The desired case. Create the directory with the correct permissions
                    fs::DirBuilder::new()
                        .recursive(false)
                        .mode(0o770)
                        .create(&runtime_dir)
                        .unwrap();
                    let pid_filename = Server::pid_file()?;
                    let mut pid_file = fs::OpenOptions::new()
                        .create_new(true)
                        .write(true)
                        .mode(0o440)
                        .open(&pid_filename).unwrap();
                    pid_file.write(process::id().to_string().as_bytes())?;
                    // File closed upon leaving scope
                },
                _ => {
                    return Err(Box::new(error));
                }
            }
        }
    }
    if let Err(error) = init_signal_handler() {
        return Err(Box::new(Error::CouldNotStartSignalHandler(error))); 
    }
    if let Err(error) = init_ipc_server() {
        return Err(Box::new(Error::CouldNotStartIPCHandler(error)));
    }
    unsafe {
        status.state = ServerState::Running;
    }

    println!("{}", Server::STARTING_MESSAGE);
    // START THE SERVER
    let addr: SocketAddr; 
    {
        let config = CONFIG.read().unwrap();
        let ipv4_address = &config.ipv4_address;
        addr = SocketAddr::from_str(ipv4_address).unwrap();
    }

    // Bind to the port and listen for incoming TCP connections
    let listener = TcpListener::bind(addr).await?;
    println!("Listening on http://{addr}");
    // TODO: Need to have other tasks be handled in this loop.
    loop {
        // When an incoming TCP connection is received grab a TCP stream for
        // client<->server communication.
        let (stream, _) = listener.accept().await?;

        // Spin up a new task in Tokio so we can continue to listen for new TCP connection on the
        // current task without waiting for the processing of the HTTP1 connection we just received
        // to finish
        tokio::task::spawn(async move {
            // Handle the connection from the client using HTTP1 and pass any
            // HTTP requests received on that connection to the `hello` function
            if let Err(err) = http1::Builder::new()
                .serve_connection(stream, service_fn(handle_request))
                .await
            {
                println!("Error serving connection: {:?}", err);
            }
        });
    }
}

#[repr(u64)]
enum AuthFlags {
    ViewBelgradeDocuments = 0x1,
    EditUsers = 0x2,
}

impl From<AuthFlags> for u64 {
    fn from(value: AuthFlags) -> Self {
        return value as u64;
    }
}

impl BitOr for AuthFlags {
    type Output = u64;
    fn bitor(self, rhs: Self) -> Self::Output {
        return self as u64 | rhs as u64;
    }
}

// An async function that consumes a request, does nothing with it and returns a
// response.
async fn handle_request(request: Request<hyper::body::Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
    use hyper::Method;
    let uri = request.uri();
    let authorization;
    if uri.path().starts_with("/belgrade/documents") {
        let flags = AuthFlags::EditUsers | AuthFlags::EditUsers;
        authorization = check_authorization(&request, flags)
    } else {
        authorization = Ok(());
    }
    if let Err(_) = authorization {
        return Ok(Response::builder()
            .status(StatusCode::FORBIDDEN)
            .body(Full::new(Bytes::from("Not authorized")))
            .unwrap()); 
    }
    let identifier = (request.method(), request.uri().path());
    println!("{:#?}", identifier);
    let response: Result<Response<Bytes>, Response<Bytes>> = match identifier {
        // (&Method::GET, "/belgrade/documents/search")       => handle_query(&request, &mut db, &request.query, &config.content_root_dir, &config.domain_name),
        // (&Method::GET, "/belgrade/documents")              => get_document_search(&request.location, &request.query, request.headers.get("Cookie"), &config.domain_name, &config.content_root_dir, &mut db),
        (&Method::GET, "/login")                              => get_login(),
        // (&Method::GET, "/api/user/login")                  => check_login(&request.query, &request.headers.get("Referer"), &config.domain_name, &mut db),
        // (&Method::GET, "/api/user/change-password")        => todo!("Need to do this"), //change_password(&request.query, &request.headers, &mut db),
        // (&Method::GET, "/api/belgrade/documents/exists")   => document_exists(&request.query, &mut db),
        // (&Method::POST,"/api/belgrade/documents")          => handle_post(&request, &mut db, &config.content_root_dir),
        #[cfg(feature = "full-server")]
        (&Method::GET, _) => fetch_content(&request),
        _ => Err(gen_response(StatusCode::NOT_FOUND, "404\nThis URL was not found on this server")) 
    };
    let vec = Vec::<u8>::new();
    let response = match response {
        Ok(response) => response,
        Err(response) => response,
    };
    let (parts, body) = response.into_parts();
    let response = Response::from_parts(parts, Full::new(body));
    return Ok(response);
}

fn check_authorization<T>(request: &Request<T>, authorization_flags: u64) -> Result<(),()> {
    return Ok(()); 
}


#[derive(Debug)]
pub enum Error {
    ServerAlreadyStarted(ServerStatus),
    CouldNotStartServer(String),
    ConfigParseError(String, Box<dyn error::Error>),
    CannotFindExecutable,
    InaccessibleSharedConfig(Box<dyn error::Error>),
    InaccessibleUserConfig(Box<dyn error::Error>),
    UnknownUsage(String),
    MalformedCmdline(String),
    InvalidIPCReponse(IPCResponse, IPCResponse),
    CouldNotConnectToSocket(Box<dyn error::Error>),
    CouldNotStartSignalHandler(Box<dyn error::Error>),
    CouldNotStartIPCHandler(Box<dyn error::Error>),
    NoHomeEnvironmentVariable,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::ServerAlreadyStarted(current_status) => {
                write!(f, "cannot start a server that is already running. 
                    Server status at time of command: {}", current_status.to_string())
            },
            Error::CannotFindExecutable => {
                write!(f, "Could not find executable `{}` in working directory: {}", EXECUTABLE_NAME, env::current_dir().unwrap().display())
            },
            Error::InaccessibleSharedConfig(souce_error) => {
                write!(f, "Could not access the shared config. Reason: {}", souce_error.to_string())
            },
            Error::InaccessibleUserConfig(source_error) => {
                write!(f, "Could not access the user config. Reason: {}", source_error.to_string())
            },
            Error::MalformedCmdline(token) => {
                write!(f, "`{}` is not a valid token", token)
            },
            Error::InvalidIPCReponse(expected, actual) => {
                write!(f, "IPC (Inter-Process Communication) Response was invalid. Expected: {:?}. Received: {:?}", expected, actual)
            },
            Error::CouldNotStartSignalHandler(source_error) => {
                write!(f, "Could not initialize the signal handler. Reason: {}", source_error.to_string())
            },
            Error::CouldNotStartIPCHandler(source_error) => {
                write!(f, "Could not initialize the IPC handler. If this is because of a missing file, it can sometimes be fixed by running `rmc server repair`. Reason: {}", source_error.to_string())
            },
            Error::NoHomeEnvironmentVariable => {
                write!(f, "Could not find $HOME")
            },
            Error::CouldNotConnectToSocket(source_error) => {
                write!(f, "Could not connect to socket file. Reason: {}", source_error.to_string())
            },
            Error::UnknownUsage(cmdline) => {
                write!(f, "The command: `{cmdline}` is not known to this program")
            },
            Error::CouldNotStartServer(reason) => {
                write!(f, "Could not start the server: Reason: {reason}")
            },
            Error::ConfigParseError(location, source_error) => {
                write!(f, "Error when parsing config at: {location}. Error: {source_error}")
            }
        } 
    }

}

impl error::Error for Error {}

#[derive(Serialize, Deserialize, Clone, Copy)]
pub enum IPCCommand {
    GetStatus,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum IPCResponse {
    CannotConnect,
    Status(ServerStatus),
}

pub struct Server {

}

impl Server {
    pub const STARTING_MESSAGE: &str = "Initialized. Starting server...";
    pub async fn exec_ipc_message(command: &IPCCommand) -> Result<IPCResponse, Box<dyn error::Error>> {
        let socket_file = Server::socket_file()?;
        let socket = UnixStream::connect(socket_file);
        if let Err(error) = socket {
            match error.kind() {
                io::ErrorKind::NotFound => {
                    return Ok(IPCResponse::CannotConnect);
                },
                _ => {
                    return Err(Box::new(Error::CouldNotConnectToSocket(Box::new(error))));
                }
            }
        }
        let mut socket = socket.unwrap();
        let string = serde_json::to_string(command)?;
        socket.write(string.as_bytes())?;

        let mut buffer = vec![0u8; 4096];
        let _bytes_read = socket.read(&mut buffer)?;
        let mut deserializer = serde_json::Deserializer::from_slice(&buffer);
        let response = IPCResponse::deserialize(&mut deserializer)?;
        return Ok(response); 
    }
    

    /// Used for config files that are shared across the whole system
    pub const SYSTEM_CONFIG_DIR: &str = "/etc/rmc";

    pub fn pid_file() -> Result<String, Box<dyn error::Error>> {
        let runtime_dir = Server::runtime_dir()?;
        return Ok(format!("{}/pid", runtime_dir));
    }

    pub fn socket_file() -> Result<String, Box<dyn error::Error>> {
        let runtime_dir = Server::runtime_dir()?;
        return Ok(format!("{}/socket", runtime_dir));
    }

    /// Used for runtime files like sockets and PIDs
    pub fn runtime_dir() -> Result<String, Box<dyn error::Error>> { 
        return Ok("/tmp/rmc".to_string());
        // match env::var("XDG_RUNTIME_DIR") {
            // Ok(xdg_runtime_dir) => {
                // return Ok(format!("{}/rmc", xdg_runtime_dir));
            // },
            // Err(error) => {
                // todo!("Implement fallback directory if there is no $XDG_RUNTIME_DIR");
            // }
        // };
    }

    /// Returns a directory where user config files are stored
    pub fn user_config_dir() -> Result<String, Box<dyn error::Error>> { 
        match env::var("XDG_CONFIG_HOME") {
            Ok(xdg_config_home) => {
                return Ok(format!("{}/rmc", xdg_config_home));
            },
            Err(_) => {
                match env::var("HOME") {
                    Ok(home) => {
                        return Ok(format!("{}/.config/rmc", home));
                    },
                    Err(_) => {
                        return Err(Box::new(Error::NoHomeEnvironmentVariable));
                    },
                };
            }
        };
    }
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize, Clone)]
pub enum ServerState {
    Placeholder,
    Unreachable,
    Stopped,
    Starting,
    Running,
    Terminating,
}

// TODO: Implement a formatter for ServerStatus
#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Clone)]
pub struct ServerStatus {
    pub state: ServerState,    
}

impl default::Default for ServerStatus {
    fn default() -> Self {
        return ServerStatus {
            state: ServerState::Unreachable
        };
    }
}

impl fmt::Display for ServerStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:#?}", self)    
    }
}

impl ServerStatus {
    pub const UNREACHABLE: ServerStatus = ServerStatus {
        state: ServerState::Unreachable,
    };
}

fn init_signal_handler() -> Result<(), Box<dyn std::error::Error>> {
    let signals = Signals::new(&[
        SIGINT, SIGTERM, SIGHUP,
    ])?;
    let handle = signals.handle();
    let signal_handler = tokio::spawn(signal_handler(signals));
    return Ok(());
}

async fn signal_handler(mut signals: Signals) {
    // The signal handler will often either add a task to the thread pool or
    // Simply return information the server is using to process things
    info!("Started signal handler");
    while let Some(signal) = signals.next().await {
        match signal {
            SIGHUP => {
                todo!("Reload the configuration file");
            }
            SIGTERM | SIGINT => {
                info!("Received signal to shutdown server");
                unsafe {
                    status.state = ServerState::Terminating;
                }
                let runtime_dir = Server::runtime_dir().unwrap();
                fs::remove_dir_all(runtime_dir).unwrap();
                println!("Deleted the runtime directory");
                std::process::exit(1);
            },
            _ => unreachable!(),
        }
    }
}

/// Creates a server which will handle inter-process communication
fn init_ipc_server() -> Result<(), Box<dyn std::error::Error>> {
    let runtime_dir = Server::runtime_dir()?;
    let listener = UnixListener::bind(format!("{}/socket", runtime_dir))?;
    let ipc_thread = tokio::spawn(ipc_handler(listener));
    Ok(())
}

async fn ipc_handler(listener: UnixListener) {
    // accept connections and process them, spawning a new thread for each one
    for stream in listener.incoming() {
        match stream {
            Ok(mut stream) => {
                println!("Connected!");
                let mut buffer = vec![0u8; 4096];
                let bytes_read = stream.read(&mut buffer).unwrap();
                let mut deserializer = serde_json::Deserializer::from_slice(&buffer);
                let command = IPCCommand::deserialize(&mut deserializer).unwrap();

                let response: IPCResponse = match command {
                    IPCCommand::GetStatus => {
                        // TODO: Running is just a placeholder. This should return the actual server status
                        unsafe {
                            IPCResponse::Status(status.clone())
                        }
                    },
                };
                let string = serde_json::to_string(&response).unwrap();
                stream.write(string.as_bytes()).unwrap();
                println!("Dispatch completed");
            }
            Err(err) => {
                /* connection failed */
                println!("Connection failed for some reason: {}", err.to_string());
                break;
            }
        }
    }
}

//////////////////////////////////////////////////////////////////////////////////////////
// EVERYTHING BELOW WAS CREATED BEFORE THE REFACTOR //////////////////////////////////////
//////////////////////////////////////////////////////////////////////////////////////////

// The various types of PDF which are processed
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum PDFType {
    Unknown = 0,
    BatchWeight = 1,
    DeliveryTicket = 2,
}

// Stores PDF metadata
#[derive(Debug)]
struct PDFMetadata {
    pdf_type: PDFType,
    // NOTE: Creating a timezone for Atlantic Standard Time (UTC-4). This will prevent having to import chrono-tz package
    datetime: DateTime<Utc>,
    customer: String,
    relative_path: String,
    doc_number: i32,
    crc32_checksum: u32,
}

// Finds the index of the first predicate (the byte to be searched for) in an
// array of bytes. Searches over a specified range. Returns None if the
// predicate cannot be found.
fn u8_index_of(array: &[u8], predicate: u8, start_index: usize, end_index: usize) -> Option<usize> {
    let index = array[start_index .. end_index]
        .iter()
        .position(|&pred| pred == predicate);
    if index.is_none() {
        return None;
    } else {
        let index = index.unwrap() + start_index;
        return Some(index)
    }
}

// Finds the index of the first predicate (the array of bytes to be searched
// for) in an array of bytes. Searches over a specified range. Returns None if
// the predicate cannot be found.
fn u8_index_of_multi(array: &[u8], predicate: &[u8], start_index: usize, end_index: usize) -> Option<usize> {
    let index = array[start_index .. end_index]
        .windows(predicate.len())
        .position(|pred| pred == predicate);
    if index.is_none() {
        return None;
    } else {
        let index = index.unwrap() + start_index;
        return Some(index);
    }
}

// This is a helper function to generate / create a simple response with status
// and message. The message parameter is generic, and can take any type which
// implements the Into<Bytes> trait.
fn gen_response<T: Into<Bytes>>(status_code: StatusCode, message: T) -> Response<Bytes> {
    let response = Response::new(Into::<Bytes>::into(message));
    let (mut parts, body) = response.into_parts();
    parts.status = status_code;
    return Response::from_parts(parts, body);
}

fn file_open(filepath: &str) -> Result<fs::File, Response<Bytes>> {
    let file: Result<File, io::Error> = File::open(filepath);
    if let Err(_) = file {
        let filename = filepath.split('/').last();
        if let Some(filename) = filename {
            return Err(gen_response(StatusCode::NOT_FOUND, format!("{filename} could not be found")));
        } else {
            return Err(Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Bytes::from(format!("{filepath} could not be found")))
                .unwrap());
        }
    }
    let file = file.unwrap();
    return Ok(file);
}
// Reads all bytes from a file. Returns a response if it was unable to.
// fn read_all_bytes(filepath: &str) -> Result<Vec<u8>, Response<Bytes>> {

//     let mut contents: Vec<u8> = Vec::new();
//     if let Err(error) = file.read_to_end(&mut contents) {
//         return Err(gen_response(StatusCode::INTERNAL_SERVER_ERROR, format!("Could not read file. Reason: {error}")));
//     }
//     Bytes::from(&contents[..])
//     return Ok(contents);
// }

// Return the static login webpage. ASSUMES that login.html is well-formced.
// WARNING: This reads the entire login.html file into a vector. It is possible
// - if this file were maliciously large - for the file to take up all memory
// and swap space on the computer. HOWEVER, if one could modify login.html,
// then there are much bigger problems afoot.
fn get_login() -> Result<Response<Bytes>, Response<Bytes>> {
    let config = &CONFIG;
    let config_reader = CONFIG.read().unwrap();
    let root_dir = &config_reader.web_content_root_dir;
    let mut file = file_open(&format!("{root_dir}/login.html"))?;
    let mut contents: Vec<u8> = Vec::new();
    if let Err(error) = file.read_to_end(&mut contents) {
        return Err(gen_response(StatusCode::INTERNAL_SERVER_ERROR, format!("Could not read file. Reason: {error}")));
    }
    let response = Response::new(Bytes::from(contents));
    // let (mut parts, body) = response.into_parts();
    // Need to set the location appropriately
    return Ok(response);
}

fn fetch_content<'a, T>(request: &Request<T>) -> Result<Response<Bytes>, Response<Bytes>>
{
    let root;
    {
        let config = CONFIG.read().unwrap();
        root = config.web_content_root_dir.clone();
    }
    let uri = request.uri();
    let content_path;
    if uri.path().eq("/") {
        content_path = format!("{root}/index.html");
    } else if uri.path().contains(".") { // FIXME: This is not a foolproof check
        content_path = format!("{root}{uri}") 
    } else {
        if uri.path().ends_with("/") {
            content_path = format!("{root}{uri}index.html") 
        } else {
            content_path = format!("{root}{uri}/index.html")
        }
    }
    println!("Path: {content_path}");
    let mut file = file_open(&content_path)?;
    let mut growing_buffer = Vec::new();
    if let Err(_) = file.read_to_end(&mut growing_buffer) {
        return Err(gen_response(StatusCode::INTERNAL_SERVER_ERROR, format!("COul")))
    }

    let include_files = get_html_includes(&growing_buffer[..])?;
    let generated_html = insert_html_includes(&growing_buffer[..], &include_files[..])?;
       
    let response = Response::builder()
        .status(StatusCode::OK)
        .body(Bytes::from(Box::from(generated_html.as_ref())));
    match response {
        Ok(response) => return Ok(response),
        Err(error) => return Err(gen_response(StatusCode::INTERNAL_SERVER_ERROR, format!("Could not generate response. Reason: {error}")))
    }
}

#[derive(Debug)]
struct HtmlIncludeComment<'a> {
    start_index: usize,
    length: usize,
    include_file: &'a str,
}

/// Returns all filenames which are to be included in this struct. The returned
/// vec is guaranteed to be sorted from largest index to smallest index (so
/// that when iterating through names, you will corrupt previous indices by
/// inserting html
fn get_html_includes(buffer: &[u8]) -> Result<Rc<[HtmlIncludeComment]>, Response<Bytes>> {
    let include_prefix = "<!--#include \"".as_bytes().to_vec();

    let include_indices: Vec<usize> = buffer
        .windows(include_prefix.len())
        .enumerate()
        .filter(|&(_index, string)| string.eq(&include_prefix[..]))
        .map(|(index, _string)| index + include_prefix.len())
        .collect();

    let end_delimiter = '\"' as u8;
    let mut lengths: Vec<usize> = Vec::new();
    for &index in &include_indices {
        let end_index = buffer[index..]
            .iter()
            .position(|&char| char.eq(&end_delimiter));
        if let Some(end_index) = end_index {
            lengths.push(end_index);
        }
    }
    if include_indices.len() != lengths.len() {
        return Err(Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body(Bytes::from("Somebody didn't format the HTML include comment correctly"))
            .unwrap());
    }
    let mut include_comments: Vec<HtmlIncludeComment> = Vec::new();
    for i in 0..include_indices.len() {
        let slice = &buffer[include_indices[i]..include_indices[i]+lengths[i]]; 
        match std::str::from_utf8(slice) {
            Ok(filepath) => {
                // NOTE: The adding and subtracting done here is so that the
                // length and size include the HTML comment itself, rather than
                // just the filename
                let html_include_comment = HtmlIncludeComment {
                    start_index: include_indices[i] - include_prefix.len(),
                    length: lengths[i] + include_prefix.len() + 4,
                    include_file: filepath,
                };
                include_comments.push(html_include_comment);
            }
            Err(_) => {
                return Err(Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .body(Bytes::from("Invalid utf-8 in HTML include comment"))
                    .unwrap());
            }
        }
    }
    include_comments.sort_by(|a, b| a.start_index.cmp(&b.start_index).reverse());
    return Ok(include_comments.into());
}

fn insert_html_includes(raw_html: &[u8], include_files: &[HtmlIncludeComment]) -> Result<Rc<[u8]>, Response<Bytes>> {
    let mut html: Vec<u8> = Vec::with_capacity(raw_html.len()); 
    html.resize(raw_html.len(), 0);
    html.copy_from_slice(raw_html);
    for include_file in include_files {
        let mut external_file: File;
        {
            let config = CONFIG.read().unwrap();
            let root = &config.web_content_root_dir;
            external_file = file_open(&format!("{root}{}", include_file.include_file))?;
        };
        let mut external_content = Vec::new();
        if let Err(_) = external_file.read_to_end(&mut external_content) {
            return Err(Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Bytes::from("Error when reading contents of external file."))
                .unwrap());
        }
        let range = include_file.start_index .. include_file.start_index+include_file.length;
        html.splice(range, external_content);
    }
    return Ok(html.into())
}

// // A user has just submitted a form with his credentials. Perform the
// // cryptographic hashing and match it to data stored in the server. If the user
// // is who they say they are, give them a cookie and return them to the page they
// // tried to access. Otherwise, return them to the login page again
// fn check_login(queries: &HashMap<String, String>, referer: &Option<&String>, domain_name: &str, db: &mut Client) -> Response {
//     let user = queries.get("user");
//     if user.is_none() { return BAD_REQUEST.clone_with_message("Query must have user field".to_string()); }
//     let user = user.unwrap();
//     let password = queries.get("pass");
//     if password.is_none() { return BAD_REQUEST.clone_with_message("Query must have password field".to_string()); }
//     let password = password.unwrap();
    
//     // Get details from the database about the hash
//     let row = db.query_opt("SELECT * FROM users WHERE username = $1", &[user]);
//     if row.is_err() {
//         return INTERNAL_SERVER_ERROR.clone_with_message(format!("Could not get username from db. Error: {}", row.unwrap_err().to_string()));
//     }
//     let row = row.unwrap();
    
//     if row.is_none() {
//         // no username with that 
//         return OK.clone_with_message("User not found".to_string());
//     }
//     let row = row.unwrap();
//     let hash: String = row.get(1); // 106 characters long
//     let reset: bool = row.get(2);

//     // A password hash is the username combined with the password, with an added salt
//     let combined_password = format!("{}{}", user, password);
//     if !sha512_crypt::verify(password, &hash) {
//         return OK.clone_with_message("Password incorrect".to_string());
//     }

//     debug_println!("password matches");
//     // PASSWORD MATCHES !
//     if reset {
//         let mut response = FOUND;
//         response.add_header("Location", "/change-password".to_string());
//         return response;
//         todo!("Need to add refer_to")
//     }
//     debug_println!("No reset");
    
//     // Password is okay
//     let mut response = FOUND;
//     add_cookie(&mut response, db, true, &user);
//     debug_println!("Cookie added");
//     // Check for return address in Referer link to decide which location to redirect to
//     if let Some(referer) = referer {
//         if let Ok((_, queries)) = http::parse_location(referer) {
//             if let Some (location) = queries.get("return_to") {
//                 let location = urlencoding::decode(&location);
//                 if let Err(error) = location {
//                     response.add_header("Location", "/".to_string());
//                     return response;
//                 }
//                 let encoded_location = location.unwrap().into_owned();
//                 let location = urlencoding::decode(&encoded_location);
//                 if let Err(encoded_location) = location {
//                     response.add_header("Location", "/".to_string());
//                     return response;
//                 }
//                 debug_println!("location: {:?}", location);
//                 response.add_header("Location", location.unwrap().to_string());
//                 return response;
//             }
//         }
//     }
//     response.add_header("Location", "/".to_string());
//     return response;
// }

// // Create a 
// fn add_cookie(response: &mut Response, db: &mut Client, authenticated: bool, username: &str) -> Result<(), postgres::Error> {
//     const COOKIE_LEN: usize = 225; // This size is arbitrary
//     // This is not cryptographically secure
//     let mut cookie = [0 as u8; COOKIE_LEN];
//     for i in 0..COOKIE_LEN {
//         cookie[i] = rand::random();
//     }
//     let cookie = general_purpose::STANDARD.encode(cookie);
//     let rows_modified = db.execute("INSERT INTO cookies (cookie, username, authenticated, created) VALUES ($1, $2, $3, now());",
//             &[&cookie, &username, &authenticated]);
//     if let Err(error) = rows_modified {
//         debug_println!("ERROR: {}", error);
//         return Err(error);
//     }
//     response.add_header("Set-Cookie", format!("id={}; Path=/belgrade/", cookie));
//     Ok(())
// }

// // Returns the stylesheet
// fn get_styles(location: &str, content_root_dir: &str) -> Response {
//     let stylesheet = read_all_bytes(&format!("{}{}", content_root_dir, location));
//     let stylesheet = unwrap_either!(stylesheet);  
//     // if stylesheet.is_err() {
//     //     return stylesheet.unwrap_err();
//     // } 
//     // let stylesheet = stylesheet.unwrap();
    
//     let mut response = OK;
//     response.add_header("content-type", CSS.to_string());
//     response.add_message(stylesheet);
//     return response;
// }

// fn get_pdf(location: &str, content_root_dir: &str) -> Response {
//     let mut response = OK;
//     response.add_header("content-type", PDF.to_string());
//     let pdf = read_all_bytes(&format!("{}{}", content_root_dir, location));
//     let pdf = unwrap_either!(pdf);
//     response.add_message(pdf);
//     return response;
// }

// // Returns whether a document exists in the database
// fn document_exists(queries: &HashMap<String, String>, db: &mut Client) -> Response {
//     // Verify the appropriate queries exist
//     if !queries.contains_key("crc32") { return BAD_REQUEST.clone_with_message("This request requires a crc32 query".to_string()); }
//     let crc32_checksum = queries.get("crc32").unwrap();
//     let crc32_checksum = crc32_checksum.parse::<u32>();
//     if let Err(error) = crc32_checksum { return INTERNAL_SERVER_ERROR.clone_with_message(format!("Could not parse checksum into u32 from database. Error: {}", error.to_string())); }
//     let crc32_checksum: u32 = crc32_checksum.unwrap();
//     debug_println!("checksum: {} ... i32: {}", crc32_checksum, crc32_checksum as i32);

//     if !queries.contains_key("type") { return BAD_REQUEST.clone_with_message("This request requires a type query".to_string()); }
//     let pdf_type = queries.get("type").unwrap().parse::<i32>();
//     if let Err(error) = pdf_type { return INTERNAL_SERVER_ERROR.clone_with_message(format!("Could not parse pdf_type into i32 from database. Error: {}", error.to_string())); }
//     let pdf_type: i32 = pdf_type.unwrap();
    
//     if !queries.contains_key("num") { return BAD_REQUEST.clone_with_message("This request requires a num query".to_string()); }
//     let pdf_num = queries.get("num").unwrap().parse::<i32>();
//     if let Err(error) = pdf_num { return INTERNAL_SERVER_ERROR.clone_with_message(format!("Could not parse pdf_num into i32 from database. Error: {}", error.to_string())); }
//     let pdf_num: i32 = pdf_num.unwrap();

//     debug_println!("Checksum: {}, Type: {}, Num: {}", crc32_checksum, pdf_type, pdf_num);
//     let row = db.query_one("SELECT COUNT(*) FROM pdfs WHERE pdf_type = $1 AND pdf_num = $2 AND crc32_checksum = $3;",
//             &[&pdf_type, &pdf_num, &(crc32_checksum as i32)]);
//     if let Err(error) = row { return INTERNAL_SERVER_ERROR.clone_with_message(format!("Could not execute the document exists query on the database. Error: {}", error.to_string())); }
//     let row = row.unwrap();
//     let document_count: i64 = row.get(0);
//     return OK.clone_with_message(document_count.to_string());
// }

// // Handles an http query to the database
// fn handle_query(request: &HttpRequest, db: &mut Client, queries: &HashMap<String, String>, content_root_dir: &str, domain_name: &str) -> Response {
//     if let Err(response) =  check_authentication(&request.location, queries, request.headers.get("Cookie"), domain_name, db) {
//         return response;
//     }
//     // Ensure all fields are present and decoded. Set defaults for empty strings
//     let query = request.query.get("query");
//     if query.is_none() {return BAD_REQUEST.clone_with_message("Query must have 'query' field".to_string());}
//     let query = query.unwrap();
//     let query = urlencoding::decode(query);
//     if let Err(error) = query { return BAD_REQUEST.clone_with_message(format!("Could not decode query into UTF-8: {}", error.to_string())); }
//     let query = query.unwrap().into_owned();
//     if query.contains("\"") || query.contains("'") { return BAD_REQUEST.clone_with_message("queries cannot have the \" or ' character in them.".to_string()); }

//     let filter = request.query.get("filter");
//     if filter.is_none() {return BAD_REQUEST.clone_with_message("Query must have 'filter' field".to_string());}
//     let filter = filter.unwrap();
//     let filter = urlencoding::decode(filter);
//     if let Err(error) = filter { return BAD_REQUEST.clone_with_message(format!("Could not decode filter into UTF-8: {}", error.to_string())); }
//     let filter = filter.unwrap().into_owned();
//     if filter.contains("\"") || filter.contains("'") { return BAD_REQUEST.clone_with_message("filters cannot have the \" or ' characters in them.".to_string()); }

//     let from = request.query.get("from");
//     if from.is_none() {return BAD_REQUEST.clone_with_message("Query must have 'from' field".to_string());}
//     let from = from.unwrap();
//     let from_datetime: DateTime<Utc>;
//     if from.is_empty() {
//         let thirty_days_ago = Utc::now().checked_sub_days(chrono::Days::new(30));
//         if let Some(datetime) = thirty_days_ago {
//             from_datetime = datetime;
//         } else {
//             return INTERNAL_SERVER_ERROR.clone_with_message("Server was unable to process 'from' datetime".to_string()); // Is this even possible?
//         }
//     } else {
//         let datetime = GMTPlus4.datetime_from_str(&format!("{} 00:00:00", from), "%Y-%m-%d %H:%M:%S");
//         match datetime {
//             Ok(datetime) => from_datetime = datetime.with_timezone(&Utc),
//             Err(error) => return BAD_REQUEST.clone_with_message(format!("'from' date was improperly formatted: {}", error.to_string())),
//         }
//     }

//     let to = request.query.get("to");
//     if to.is_none() {return BAD_REQUEST.clone_with_message("Query must have 'to' field".to_string());}
//     let to = to.unwrap();
//     let to_datetime: DateTime<Utc>;
//     if to.is_empty() {
//         to_datetime = Utc::now();
//     } else {
//         let datetime = GMTPlus4.datetime_from_str(&format!("{} 23:59:59", to), "%Y-%m-%d %H:%M:%S");
//         match datetime {
//             Ok(datetime) => to_datetime = datetime.with_timezone(&Utc),
//             Err(error) => return BAD_REQUEST.clone_with_message(format!("'to' date was improperly formatted: {}", error.to_string())),
//         }
//     }

//     if to_datetime < from_datetime {
//         return BAD_REQUEST.clone_with_message("The 'from' is sooner than the 'to' date, or the from date does not exist".to_string());
//     }

//     debug_println!("Query: {} Filter: {} Processed datetimes --- From: {:?} To: {:?}", query, filter, from_datetime, to_datetime);
    
//     // Everything has been extracted and processed. Ready for database query
//     const BASE_REQUEST: &str = r#"WITH r AS ( SELECT CASE WHEN (e.customer = '') THEN c.pdf_datetime ELSE e.pdf_datetime END, pdf_num, CASE WHEN (e.customer = '') THEN c.customer ELSE e.customer END, relative_path, "dt_path" FROM ( SELECT pdf_datetime, pdf_num, customer, relative_path FROM pdfs WHERE pdf_type = 1 ) AS e FULL JOIN ( SELECT pdf_num, pdf_datetime, customer, relative_path AS "dt_path" FROM pdfs WHERE pdf_type = 2 ) AS c USING (pdf_num) ) SELECT * FROM r WHERE pdf_datetime BETWEEN $1 AND $2"#;
//     // const BASE_REQUEST: &str = "SELECT pdf_datetime, pdf_type, pdf_num, customer, relative_path FROM pdfs WHERE pdf_datetime BETWEEN $1 AND $2";
//     let full_query = match filter.as_str() {
//         "Customer" => {
//             format!("{} AND customer ILIKE '%{}%' ORDER BY pdf_num DESC;", BASE_REQUEST, query)
//         }
//         "Number" => {
//             let num = if let Ok(number) = query.parse::<u32>() { number } else { return BAD_REQUEST.clone_with_message("A valid number was not included in the search".to_string())};
//             format!("{} AND pdf_num = {};", BASE_REQUEST, num )
//         },
//         _ => { 
//             format!("{} AND relative_path ILIKE '%{}%' ORDER BY pdf_num DESC;", BASE_REQUEST, query)
//         }
//     };
    
//     // Execute query
//     debug_println!("Query to be executed: {}", full_query);
//     let rows = db.query(&full_query, &[&from_datetime, &to_datetime]);
//     if let Err(error) = rows { return INTERNAL_SERVER_ERROR.clone_with_message(format!("Could not execute query on database: {}", error.to_string())); }
//     let rows = rows.unwrap();
    
//     // Create HTML table in response
//     let entries = rows.len();
//     let mut table = format!("<p>Found {} entries</p><table><tr><th>DateTime</th><th>Num</th><th>Customer</th><th>Batch Weights</th><th>Delivery Ticket</th></tr>", entries);
//     for row in rows {
//         let datetime: DateTime<Utc> = row.get(0);
//         let pdf_num: i32 = row.get(1);
//         let customer: &str = row.get(2);
//         let bw_path: String = if let Ok(path) = row.try_get::<_, &str>(3) { format!("<a href=\"{}/belgrade/documents/{}\">Weights</a>", domain_name, path) } else { String::new() };
//         let dt_path: String = if let Ok(path) = row.try_get::<_, &str>(4) { format!("<a href=\"{}/belgrade/documents/{}\">Ticket</a>", domain_name, path) } else { String::new() };
//         // debug_println!("Datetime: {:?}, PDF Type: {}, Num: {}, Customer: {} Path: {}", datetime, pdf_type, pdf_num, customer, relative_path);

//         let table_row = format!("<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
//                 datetime.with_timezone(&GMTPlus4).format("%Y-%b-%d %I:%M %p").to_string(), pdf_num, customer, bw_path, dt_path);
//         table.push_str(&table_row);
//     }
//     table.push_str("</table>");

//     // Read from source html file and return appended file to client
//     let mut index: Vec<u8> = vec![0;2048];
//     let mut file = File::open(format!("{}/belgrade/documents/index.html", content_root_dir));
//     if let Err(error) = file {return INTERNAL_SERVER_ERROR.clone_with_message("Could not open the index.html file".to_owned()); }
//     let mut file = file.unwrap();
//     let bytes_read = file.read(&mut index);
//     if let Err(error) = bytes_read {return INTERNAL_SERVER_ERROR.clone_with_message("Could not read from the index.html file".to_owned()); }
//     let bytes_read = bytes_read.unwrap();
//     let end = index.windows(7).position(|x| x == b"</form>");
//     if end.is_none() {return INTERNAL_SERVER_ERROR.clone_with_message("Could not parse the server's own index.html file".to_string()); }
//     let mut end = end.unwrap();
//     end += 7;

//     // Overwrite
//     let mut table = table.as_bytes().to_vec();
//     let mut index = index[..end].to_vec();
//     index.append(&mut table);
//     index.append(&mut b"</body>".to_vec());

//     let mut response = OK;
//     response.add_message(index);
//     response.add_header("content-type", HTML.to_string());
//     return response;
// }

// fn is_authenticated(cookie: &str, db: &mut Client) -> bool {
//     let row_result = db.query_opt("SELECT * FROM cookies where cookie = $1", &[&cookie]);
//     if let Ok(optional_row) = row_result {
//         if let Some(row) = optional_row {
//             return true;
//         }
//     }
//     return false;
// }

// fn authenticate(location: &str, queries: &HashMap<String, String>, domain_name: &str) -> Response {
//     let mut response = FOUND;
//     let mut return_to = urlencoding::encode(location).to_string();
//     if queries.len() > 0 {
//         return_to.push('?');
//         for (key, value) in queries {
//             return_to.push_str(&format!("{}={}&", key, value));
//         }
//     }
//     let return_to = if queries.len() > 0 {
//         urlencoding::encode(&return_to[..return_to.len()-1])
//     } else {
//         urlencoding::encode(&return_to)
//     };
//     response.add_header("Location", format!("/login?return_to={}", return_to));
//     response.add_header("content-type", HTML.to_string());
//     return response;
// }

// fn extract_cookie(cookie: &str) -> Option<&str> {
//     // This is just hard coded right now.
//     if cookie.len() > 3 {
//         return Some(&cookie[3..]);
//     } else {
//         return None;
//     }
// }

// fn check_authentication(location: &str, queries: &HashMap<String, String> ,cookie: Option<&String>, domain_name: &str, db: &mut Client) -> Result<(), Response> {
//     if let Some(cookie) = cookie {
//         if let Some(cookie) = extract_cookie(cookie) {
//             if !is_authenticated(cookie, db) {
//                 return Err(authenticate(location, queries, domain_name));
//             } else { // This is the only safe path 
//                 return Ok(())
//             }
//         } else {
//             return Err(authenticate(location, queries, domain_name));
//         }
//     } else {
//         return Err(authenticate(location, queries, domain_name));
//     }
// }

// // Handles a get to belgrade
// fn get_document_search(location: &str, queries: &HashMap<String, String>, cookie: Option<&String>, domain_name: &str, content_root_dir: &str, db: &mut Client) -> Response {
//     if let Err(response) =  check_authentication(location, queries, cookie, domain_name, db) {
//         return response;
//     }

//     let path = format!("{}{}/index.html", content_root_dir, location);
//     debug_println!("{}", path);
//     let page = read_all_bytes(&path);  
//     if page.is_err() {
//         return page.unwrap_err();
//     } 
//     let page = page.unwrap();
    
//     let mut response = OK;
//     response.add_header("content-type", HTML.to_string());
//     response.add_message(page);
//     return response;
// }

// // Handles a POST Http request. This is typically where PDFs are received,
// // analyzed, and sorted into the correct location. Returns a success or error
// // message.
// fn handle_post(request: &HttpRequest, db: &mut Client, content_root_dir: &str) -> Response {
//     // NOTE: Assumes that PDF is sent unmodified in Body. Currently, the minimum
//     // required metadata for storing a PDF will be the date, customer, and
//     // pdf-type (Delivery Ticket or Batch Weight).
//     let pdf_as_bytes = request.body.single().unwrap();
//     // let pdf_as_bytes = request.body.unwrap().single().unwrap();

//     // Confirm that this file is indeed a PDF file
//     if !pdf_as_bytes.starts_with(b"%PDF") {
//         return BAD_REQUEST.clone_with_message("PDF File not detected: The PDF version header was not found".to_owned());
//     }

//     // Decide whether Batch Weight or Delivery Ticket or Undecidable.
//     let pdf_type: PDFType;
//     let id = [b"/Widths [", CR, LF, b"600 600 600 600 600 600 600 600 600"].concat();
//     let id = id.as_slice();
//     if let Some(result) = 
//             pdf_as_bytes
//                 .windows(id.len())
//                 .find(|&pred| pred
//                 .eq(id)) {
//         pdf_type = PDFType::BatchWeight;
//     } else {
//         let id = [b"/Widths [", CR, LF, b"277 333 474 556 556 889 722 237 333"].concat();
//         let id = id.as_slice();
//         if let Some(result) = 
//                 pdf_as_bytes
//                     .windows(id.len())
//                     .find(|&pred| pred
//                     .eq(id)) {
//             pdf_type = PDFType::DeliveryTicket; 
//         } else {
//             pdf_type = PDFType::Unknown;
//         }
//     } 

//     // Deflate and retrieve date and customer.
//     let mut date = String::new();
//     let mut customer = String::new();
//     let mut doc_number = 0;
//     let mut time = String::new();
//     let LENGTH_PREFIX = b"<</Length ";
//     let mut i = 0;
//     while let Some(flate_header) = u8_index_of_multi(pdf_as_bytes.as_slice(), LENGTH_PREFIX, i, pdf_as_bytes.len()) {
//         if pdf_type == PDFType::Unknown {break;}
//         // Get line which sets up Flate decode and extract the length from it
//         let length_start_index = flate_header + LENGTH_PREFIX.len();
//         let length_end_index = u8_index_of(&pdf_as_bytes, b'/', length_start_index, pdf_as_bytes.len()).unwrap();
//         let length = pdf_as_bytes[length_start_index..length_end_index].to_vec();
//         let digits: Vec<u8> = 
//             length
//                 .iter()
//                 .map(|&c| c - 48)
//                 .collect();
//         let length = digits.from_digits() as usize;
//         let stream_start_index = u8_index_of(&pdf_as_bytes, CR[0], length_end_index, pdf_as_bytes.len());
//         if stream_start_index == None {
//             return BAD_REQUEST.clone_with_message("Could not find the start of the Flate Encoded Stream. FlateStream should be prefaced by a CRLF pattern, which was not detected. This can occur when the data is not sent as a binary file.".to_string());
//         }
//         let stream_start_index = stream_start_index.unwrap() + 2; //NOTE: The unwrap is safe, the +2 is not
//         let stream_end_index = stream_start_index + length;
//         i = stream_end_index;
//         let stream = &pdf_as_bytes[stream_start_index..stream_end_index];
//         let mut output_buffer = String::new();
//         debug_println!("=======CHECKPOINT=========");
//         zlib::Decoder::new(stream).read_to_string(&mut output_buffer);
//         // debug_println!("zlib output: {:?}", &output_buffer);
//         // debug_println!("Stream start: {} End: {} Size: {} Length: {}", stream_start_index, stream_end_index, stream.len(), length);
//         // FIXME: This will break when a key, value pair is along a boundary
//         let DATE_PREFIX = if pdf_type == PDFType::DeliveryTicket {"Tf 480.8 680 Td ("} else {"BT 94 734 Td ("}; // NOTE: This should be a const, and is used improperly
//         let date_pos = output_buffer.find(DATE_PREFIX);
//         if let Some(mut date_pos) = date_pos {
//             date_pos += DATE_PREFIX.len();
//             let date_end_pos = u8_index_of_multi(&output_buffer.as_bytes(), b")Tj", date_pos, output_buffer.len()).unwrap();
//             date = output_buffer[date_pos..date_end_pos].to_string(); //NOTE: DANGEROUS 
//         }

//         let DOC_NUM_PREFIX = if pdf_type == PDFType::DeliveryTicket {"Tf 480.8 668.8 Td ("} else {"BT 353.2 710 Td ("};
//         let doc_num_pos = output_buffer.find(DOC_NUM_PREFIX);
//         if let Some(mut doc_num_pos) = doc_num_pos {
//             doc_num_pos += DOC_NUM_PREFIX.len();
//             let doc_num_end_pos = u8_index_of_multi(&output_buffer.as_bytes(), b")Tj", doc_num_pos, output_buffer.len()).unwrap();
//             doc_number = output_buffer[doc_num_pos..doc_num_end_pos].parse().unwrap(); //NOTE: DANGEROUS 
//         }

//         let TIME_PREFIX = if pdf_type == PDFType::DeliveryTicket {""} else {"BT 353.2 686 Td ("}; // NOTE: This should be a const, and is used improperly
//         let time_pos = output_buffer.find(TIME_PREFIX);
//         if time_pos != None && pdf_type == PDFType::BatchWeight {
//             let time_pos = time_pos.unwrap() + TIME_PREFIX.len();
//             let time_end_pos = u8_index_of_multi(&output_buffer.as_bytes(), b")Tj", time_pos, output_buffer.len()).unwrap();
//             time = output_buffer[time_pos..time_end_pos].to_string(); //NOTE: DANGEROUS 
//         }
        
//         let CUSTOMER_PREFIX = if pdf_type == PDFType::DeliveryTicket {"Tf 27.2 524.8 Td ("} else {"BT 94 722 Td ("};
//         let customer_pos = output_buffer.find(CUSTOMER_PREFIX);
//         if let Some(mut customer_pos) = customer_pos {
//             customer_pos += CUSTOMER_PREFIX.len();
//             let customer_end_pos = u8_index_of_multi(&output_buffer.as_bytes(), b")Tj", customer_pos, output_buffer.len()).unwrap();
//             customer = output_buffer[customer_pos..customer_end_pos].to_string();
//         } 
//     }

//     // Parse string date formats into Chrono (ISO 8601) date formats. Delivery
//     // Tickets all currently have their DateTimes set to 12 noon EST, since
//     // extracting their datetimes is hard and cannot be done yet. More info in
//     // issue 3 on GitHub.
//     // FIXME: Improper handing of errors here can crash the program
//     let mut datetime = Utc.timestamp_nanos(0);
//     if pdf_type == PDFType::BatchWeight {
//         let combined = format!("{} {}", &date, &time);
//         debug_println!("date: {}, time: {}, combined: {}", &date, &time, &combined);
//         let dt = GMTPlus4.datetime_from_str(&combined, "%e-%b-%Y %I:%M:%S %p");
//         if let Ok(dt) = dt {
//             datetime = dt.with_timezone(&Utc);
//         } else {
//             datetime = Utc.with_ymd_and_hms(1970, 1, 1, 12, 0, 0).unwrap();
//         }
//     } else if pdf_type == PDFType::DeliveryTicket {
//         let combined = format!("{} 12:00:00", &date);
//         debug_println!("date: {}", &combined);
//         let dt = GMTPlus4.datetime_from_str(&combined, "%d/%m/%Y %H:%M:%S");
//         if let Ok(dt) = dt {
//             datetime = dt.with_timezone(&Utc);
//         } else {
//             datetime = Utc.with_ymd_and_hms(1970, 1, 1, 12, 0, 0).unwrap();
//         } 
//     } else {
//         datetime = Utc.with_ymd_and_hms(1970, 1, 1, 12, 0, 0).unwrap();
//     }

//     // Generate a relative filepath (including filename) of the PDF. Files will be sorted in folders by years and then months
//     let result_row = db.query(
//         "SELECT COUNT(*) FROM pdfs WHERE CAST(pdf_datetime as DATE) = $1 AND pdf_num = $2;",
//         &[&datetime.date_naive(), &(doc_number as i32)] // NOTE: This cast is redundant, but VSCode thinks it is an error without it. Rust does not. It compiles and runs and passes testcases.
//     );
//     if let Err(e) = result_row {
//         return INTERNAL_SERVER_ERROR.clone_with_message(e.to_string());
//     }
//     let result_row = result_row.unwrap();
//     let num_entries: i64 = result_row[0].get(0);
    
//     let duplicate = if num_entries == 0 {String::new()} else {format!("_{}",num_entries.to_string())}; // There should only ever be one entry for this, but should a duplicate arise this handles it.
//     let type_initials = if pdf_type == PDFType::DeliveryTicket {"DT"} else if pdf_type == PDFType::BatchWeight {"BW"} else {"ZZ"};
//     let relative_filepath = format!("{}_{}_{}{}{}.pdf",datetime.format("%Y/%b/%d").to_string(), customer, type_initials, doc_number, duplicate); // eg. 2022/Aug/7_John Doe_DT154.pdf
//     let crc32 = Crc::<u32>::new(&crc::CRC_32_ISO_HDLC);
//     let checksum = crc32.checksum(&pdf_as_bytes);

//     let pdf_metadata = PDFMetadata { 
//         datetime:       datetime, // NOTE: As of Dec 21 2022, this date uses a different format in Batch Weights vs Delivery Tickets
//         pdf_type:       pdf_type,
//         customer :      customer,
//         relative_path:  relative_filepath,
//         doc_number:     doc_number,
//         crc32_checksum: checksum,
//     };
    
//     // Check whether pdf with this metadata already exists in database
//     let row = db.query_one("SELECT COUNT(*) FROM pdfs WHERE crc32_checksum = $1 AND pdf_num = $2 AND pdf_type = $3;",
//             &[&(pdf_metadata.crc32_checksum as i32), &pdf_metadata.doc_number, &(pdf_metadata.pdf_type as i32)]);
//     if let Err(error) = row {return INTERNAL_SERVER_ERROR.clone_with_message(format!("Was not able to check if file already existed in server. Error: {}", error.to_string())) ;}
//     let row = row.unwrap();
//     if row.len() < 1 {return INTERNAL_SERVER_ERROR.clone_with_message("Tried to check if file already existed in database before adding it. Result from SQL query had no response when one was expected.".to_string()); }
//     let count: i64 = row.get(0);
//     debug_println!("Count: {}", count);
//     if count > 0 {
//         return OK.clone_with_message("File already exists in server. Taking no action.".to_string());
//     } 

//     // Place the PDF file into the correct place into the filesystem
//     {
//         let path_string = format!("{}{}{}", content_root_dir, "/belgrade/documents/", &pdf_metadata.relative_path);
//         let path = Path::new(&path_string);
//         let prefix = path.parent().unwrap(); // path without final component
//         debug_println!("Prefix: {:?}", prefix);
//         fs::create_dir_all(prefix).unwrap();
//         let mut pdf_file = File::create(&path_string).unwrap();
//         pdf_file.write_all(pdf_as_bytes.as_slice()).unwrap();
//     }
    
//     debug_println!("METADATA: {:#?}", pdf_metadata);

//     // Store PDF into Database
//     db.query(concat!("INSERT INTO pdfs (pdf_type, pdf_num, pdf_datetime, customer, relative_path, crc32_checksum)",
//             "VALUES ($1, $2, $3, $4, $5, $6);"),
//             &[&(pdf_metadata.pdf_type as i32),
//             &pdf_metadata.doc_number,
//             &datetime, 
//             &pdf_metadata.customer,
//             &pdf_metadata.relative_path,
//             &(pdf_metadata.crc32_checksum as i32)]
//     );


//     // This is where the PDF should be parsed
//     return CREATED.clone_with_message("PDF received and stored on server succesfully".to_owned());
// }

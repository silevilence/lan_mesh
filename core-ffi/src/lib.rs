//! JNI entry points for the Android Leaf client.
mod client;

use client::{Client, EventSink, OpenGroup};
use jni::{
    JNIEnv, JavaVM,
    objects::{GlobalRef, JObject, JString, JValue},
    sys::{jlong, jstring},
};
use lan_mesh::{DeviceId, FileId, Session};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    error::Error,
    net::SocketAddr,
    path::PathBuf,
    ptr,
    sync::{
        Arc, LazyLock, Mutex,
        atomic::{AtomicI64, Ordering},
    },
    time::Duration,
};
use tokio::runtime::Runtime;

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
static RUNTIME: LazyLock<Runtime> = LazyLock::new(|| Runtime::new().expect("Tokio runtime"));
// The registry only owns JNI handles. Networking, events and transfers belong to each Client.
static HANDLES: LazyLock<Mutex<HashMap<i64, Arc<Client>>>> = LazyLock::new(Mutex::default);
static NEXT_HANDLE: AtomicI64 = AtomicI64::new(1);

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Command {
    Discover,
    Probe {
        address: SocketAddr,
    },
    Members,
    Text {
        peer: Option<DeviceId>,
        content: String,
    },
    File {
        peer: Option<DeviceId>,
        path: PathBuf,
        file_id: FileId,
    },
    Reconnect,
}

fn callback(vm: JavaVM, listener: GlobalRef) -> EventSink {
    Arc::new(move |event| {
        let mut env = vm.attach_current_thread()?;
        let text = env.new_string(event.to_string())?;
        let called = env.call_method(
            &listener,
            "onEvent",
            "(Ljava/lang/String;)V",
            &[JValue::Object(&text)],
        );
        if let Err(err) = called {
            if env.exception_check()? {
                env.exception_describe()?;
                env.exception_clear()?;
            }
            return Err(err.into());
        }
        Ok(())
    })
}

fn exception(env: &mut JNIEnv, error: Box<dyn Error + Send + Sync>) {
    if !env.exception_check().unwrap_or(true) {
        let _ = env.throw_new("java/io/IOException", error.to_string());
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_lanmesh_android_Native_open(
    mut env: JNIEnv,
    _: JObject,
    config: JString,
    listener: JObject,
) -> jlong {
    let result = (|| -> Result<_> {
        let config: String = env.get_string(&config)?.into();
        let options: OpenGroup = serde_json::from_str(&config)?;
        let emit = callback(env.get_java_vm()?, env.new_global_ref(listener)?);
        let client = RUNTIME.block_on(Client::open(options, emit))?;
        let id = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
        HANDLES.lock().unwrap().insert(id, Arc::new(client));
        Ok(id)
    })();
    result.unwrap_or_else(|e| {
        exception(&mut env, e);
        0
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_lanmesh_android_Native_call(
    mut env: JNIEnv,
    _: JObject,
    handle: jlong,
    request: JString,
) -> jstring {
    let result = (|| -> Result<_> {
        let request: String = env.get_string(&request)?.into();
        let command: Command = serde_json::from_str(&request)?;
        let value = RUNTIME.block_on(execute(handle, command))?;
        Ok(env.new_string(value.to_string())?.into_raw())
    })();
    result.unwrap_or_else(|e| {
        exception(&mut env, e);
        ptr::null_mut()
    })
}

async fn execute(handle: i64, command: Command) -> Result<Value> {
    match command {
        Command::Discover => {
            return Ok(json!(
                Session::scan_relays(([0, 0, 0, 0], 37020).into(), Duration::from_secs(3)).await?
            ));
        }
        Command::Probe { address } => return Ok(json!(Session::probe_group(address).await?)),
        _ => {}
    }
    let client = HANDLES
        .lock()
        .unwrap()
        .get(&handle)
        .cloned()
        .ok_or("群组已关闭")?;
    match command {
        Command::Members => client.members().await,
        Command::Text { peer, content } => client.send_text(peer, content).await,
        Command::File {
            peer,
            path,
            file_id,
        } => client.send_file(peer, path, file_id).await,
        Command::Reconnect => {
            client.session.reconnect_leaf().await?;
            Ok(Value::Null)
        }
        Command::Discover | Command::Probe { .. } => unreachable!(),
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_lanmesh_android_Native_close(_: JNIEnv, _: JObject, handle: jlong) {
    let client = HANDLES.lock().unwrap().remove(&handle);
    if let Some(client) = client {
        RUNTIME.block_on(client.close());
    }
}

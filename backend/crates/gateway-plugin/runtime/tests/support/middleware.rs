use gateway_plugin_sdk::{
    Capability, ContributionDeclaration, Contributions, Stage,
    client::{MiddlewareCall, MiddlewarePlugin, PluginSession, SessionConfig},
};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let session = PluginSession::accept(
        tokio::io::stdin(),
        tokio::io::stdout(),
        SessionConfig::default(),
    )
    .await?;
    let mode = session.handshake().configuration["mode"]
        .as_str()
        .unwrap_or("map")
        .to_owned();
    let configuration = session.handshake().configuration.clone();
    let plugin = MiddlewarePlugin::new(
        &Contributions::from([(
            Capability::Middleware,
            ContributionDeclaration {
                id: "test.example.middleware".into(),
                version: configuration["middleware_version"]
                    .as_u64()
                    .unwrap_or(1)
                    .try_into()?,
                stages: vec![if configuration["attempt"] == true {
                    Stage::Attempt
                } else {
                    Stage::Request
                }],
                input_formats: vec!["openai".into()],
                output_formats: vec!["openai".into()],
            },
        )]),
        move |mut call: MiddlewareCall| {
            let mode = mode.clone();
            let configuration = configuration.clone();
            async move {
                if mode == "declare" {
                    call.request
                        .replace_body(serde_json::to_vec(&configuration["body"]).unwrap());
                    call.request.declare_capabilities(
                        serde_json::from_value(configuration["capabilities"].clone()).unwrap(),
                    );
                }
                if mode == "headers" {
                    call.request
                        .append_header("x-sdk-request", b"active".to_vec());
                }
                if mode == "inspect" && !call.request.body.is_empty() {
                    // 读取和解析只能作用于副本，不能把解析结果写回未改写的正文。
                    assert!(
                        serde_json::from_slice::<serde_json::Value>(&call.request.body).is_ok()
                    );
                }
                let mut response = call.next.run(call.request).await?;
                match mode.as_str() {
                    "passthrough" | "declare" => {}
                    "inspect" => {
                        response.body = response.body.inspect_frames(|frame| {
                            assert!(!frame.payload.is_empty());
                        })?;
                    }
                    "headers" => response.append_header("x-sdk-response", b"active".to_vec()),
                    _ => {
                        response.append_header("x-sdk-plugin", b"active".to_vec());
                        response.body = response.body.map_frames(|mut frame| {
                            frame.payload.push(b' ');
                            Ok(vec![frame])
                        })?;
                    }
                }
                Ok(response)
            }
        },
    )?;
    session.run(plugin).await?;
    Ok(())
}

use core::panic;
use lettre::address::Envelope;
use lettre::message::header::{
    ContentDisposition, ContentTransferEncoding, ContentType, HeaderName, HeaderValue,
};
use lettre::message::{Body, MaybeString, MultiPart, SinglePart};
use lettre::{Message, Transport};
use std::os::unix::fs::MetadataExt;

use mailparse::MailHeaderMap;
use regex::Regex;
use std::borrow::Cow;
use std::ffi::OsString;

use std::env::VarError;
use std::fmt::Write;
use std::io::{self, Read};
use std::sync::OnceLock;
use tracing::debug;

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    sender_email: lettre::Address,
    recipient_email: lettre::Address,
    smtp_host: String,
    smtp_username: String,
    smtp_password: String,
}

fn main() {
    tracing_subscriber::fmt::init();

    debug!("loading config");
    let config_location_default = "/etc/forward-as-attachment-mta.config.toml".to_owned();
    let config_location = match std::env::var("FORWARD_AS_ATTACHMENT_MTA_CONFIG_FILE") {
        Ok(v) => v,
        Err(VarError::NotPresent) => config_location_default,
        e @ Err(VarError::NotUnicode(_)) => panic!("{e:?}"),
    };
    let config_fd = match std::fs::File::open(&config_location) {
        Ok(fd) => fd,
        Err(e) => panic!("open config file at {config_location:?}\n{e:?}"),
    };
    let config_string = match std::fs::read_to_string(&config_location) {
        Ok(c) => c,
        Err(e) => panic!("read config at {config_location:?}\n{e:?}"),
    };
    let config: Config = match toml::from_str(&config_string) {
        Ok(c) => c,
        Err(e) => panic!("{e:?}"),
    };

    enum Args {
        AllUtf8(Vec<String>),
        Lossy(Vec<String>),
    }
    let args: Args = {
        let os: Vec<std::ffi::OsString> = std::env::args_os().collect();
        let maybe_all_utf8: Result<Vec<String>, ()> = os
            .iter()
            .map(|os_str| os_str.to_str().ok_or(()).map(|s| s.to_owned()))
            .collect(); // cancels iteration early on first err
        match maybe_all_utf8 {
            Ok(all_utf8) => Args::AllUtf8(all_utf8),
            Err(_) => Args::Lossy(
                os.into_iter()
                    .map(|os_str| os_str.to_string_lossy().to_string())
                    .collect(),
            ),
        }
    };
    impl std::fmt::Display for Args {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            let (prefix, args) = match self {
                Args::AllUtf8(args) => ("", args),
                Args::Lossy(args) => ("(non-utf-8): ", args),
            };
            write!(f, "{prefix}: {args:?}",)
        }
    }
    tracing::debug!(%args, "args");

    enum OriginalMessageBody {
        Read(Vec<u8>),
        Error(std::io::Error),
    }
    let stdin_raw: OriginalMessageBody = {
        let mut stdin_content = Vec::new();
        match io::stdin().read_to_end(&mut stdin_content) {
            Ok(_) => {
                std::fs::write("/tmp/debug.eml", &stdin_content).expect("IO error");
                OriginalMessageBody::Read(stdin_content)
            }
            Err(e) => OriginalMessageBody::Error(e),
        }
    };
    impl From<OriginalMessageBody> for MaybeString {
        fn from(val: OriginalMessageBody) -> Self {
            match val {
                OriginalMessageBody::Read(b) => MaybeString::Binary(b),
                OriginalMessageBody::Error(e) => MaybeString::String(format!(
                    "forward-as-attachment-mta failed to read stdin: {e:?}"
                )),
            }
        }
    }
    let original_parsed = match &stdin_raw {
        OriginalMessageBody::Read(body_raw) => match mailparse::parse_mail(body_raw) {
            Ok(msg) => Some(msg),
            Err(_) => None,
        },
        OriginalMessageBody::Error(_) => None,
    };
    tracing::debug!(could_parse = original_parsed.is_some(), "parsed message");

    // Put together the wrapper message
    let sender = {
        // Some(unambiguous `From` header)
        let original_parsed_from = original_parsed.as_ref().and_then(|org| {
            match org.get_headers().get_all_headers("From").as_slice() {
                [unambiguous] => match mailparse::addrparse_header(unambiguous) {
                    Ok(list) => {
                        let maybe_from = list.extract_single_info();
                        debug!(?maybe_from);
                        maybe_from.map(|single_info| single_info.addr)
                    }
                    Err(e) => {
                        debug!(%e, "parse From header error");
                        // best-effort: handle typical Cron format
                        match unambiguous.get_value_utf8() {
                            Ok(unambigous) => {
                                debug!(?unambigous, "trying to parse Cron format");
                                try_extract_cron_from_header(&unambigous).map(|s| s.to_string())
                            }
                            Err(_) => None,
                        }
                    }
                },
                _ => None,
            }
        });
        let args_from: Option<String> = match args {
            Args::AllUtf8(ref args) => {
                let mut first = None;
                for arg in args {
                    // sendmail uses -f to denominate envelope-from
                    let Some(from) = arg.strip_prefix("-f") else {
                        continue;
                    };
                    if first.is_some() {
                        first = None; // duplicate from, no idea how to handle that
                        break;
                    }
                    first = Some(from.to_owned());
                }
                first
            }
            Args::Lossy(_) => None,
        };
        debug!(?original_parsed_from, ?args_from, "prepare sender");
        match (
            args_from.as_deref().map(escape_parens),
            original_parsed_from.as_deref().map(escape_parens),
        ) {
            (Some(a), Some(h)) if a == h => format!("evlp+hdr({a})"),
            (Some(a), Some(h)) => format!("evlp({a})+hdr({h})"),
            (Some(a), None) => format!("evlp({a})"),
            (None, Some(h)) => format!("hdr({h})"),
            (None, None) => "???".to_owned(),
        }
    };
    let summary = match &original_parsed {
        Some(parsed) => match parsed.get_headers().get_all_values("Subject").as_slice() {
            [unambiguous] => unambiguous.clone(),
            _x => "(multiple Subject headers)".to_owned(),
        },
        None => "(unparseable message)".to_owned(),
    };
    let hostname = hostname::get()
        .map(|os_str| os_str.to_string_lossy().to_string())
        .unwrap_or("???".to_string());

    let subject = format!("{sender}@{hostname}: {summary}");

    let body = (|| {
        let mut body = String::new();
        writeln!(
            &mut body,
            "A process on host {hostname:?} invoked the sendmail binary."
        )?;
        writeln!(
            &mut body,
            "On that host, the sendmail binary is provided by the forwad-as-attachment-mta package."
        )?;
        match config_fd.metadata() {
            Ok(md) => {
                // Rust std widens the mode bits to the biggest common type across all supported platforms.
                // https://github.com/rust-lang/rust/commit/aa23c98450063992473d40d707273903f8a3937d
                let mode = md.mode();
                let more_than_user_has_access = (mode & (libc::S_IRWXG as u32 | libc::S_IRWXO as u32)) != 0;
                if more_than_user_has_access {
                    writeln!(&mut body, "WARNING: the config file contains SMTP credentials and has too-lax permissions: {}",
                        uucore::fs::display_permissions(&md, false)
                    )?;
                }
            },
            Err(e) => {
                writeln!(&mut body, "WARNING: could not determine permissions of the config file, they may or may not be too lax: {e}")?;
            },
        }
        writeln!(
            &mut body,
            "The original message is attached inline to this wrapper message."
        )?;
        writeln!(&mut body)?;
        writeln!(&mut body, "Invocation args: {args}")?;
        writeln!(&mut body)?;
        writeln!(
            &mut body,
            "uid:{} gid:{} euid:{} egid:{}",
            users::get_current_uid(),
            users::get_current_gid(),
            users::get_effective_uid(),
            users::get_effective_gid()
        )?;
        let mut display_or_none = |what, value: Option<OsString>| {
            writeln!(
                &mut body,
                "{what}: {}",
                value
                    .as_ref()
                    .map(|s| s.to_string_lossy())
                    .unwrap_or(Cow::Borrowed(""))
            )
        };
        display_or_none("username", users::get_current_username())?;
        display_or_none("groupname", users::get_current_groupname())?;
        display_or_none("effective username", users::get_effective_username())?;
        display_or_none("effective groupname", users::get_effective_groupname())?;
        writeln!(&mut body)?;
        writeln!(&mut body, "hostname: {}", whoami::hostname())?;
        writeln!(&mut body, "device name: {}", whoami::devicename())?;
        writeln!(&mut body, "distro: {}", whoami::distro())?;
        writeln!(&mut body, "platform: {}", whoami::platform())?;
        writeln!(&mut body)?;
        std::result::Result::<_, std::fmt::Error>::Ok(body)
    })()
    .expect("this is all in-memory and we don't expect formatting to fail");

    let envelope = Envelope::new(
        Some(config.sender_email.clone()),
        vec![config.recipient_email.clone()],
    )
    .expect("as per api docs, this can't fail");
    let email_message = Message::builder()
        .from(config.sender_email.into())
        .to(config.recipient_email.into())
        .subject(subject)
        .envelope(envelope)
        .multipart({
            let mut mp_builder = MultiPart::mixed().singlepart(SinglePart::plain(body));

            // Try to create an inline attachment for the receivers's convenience of not
            // having to double-click the attachment.
            //
            // This is surprisingly tricky, as the message/rfc822 MIME type only allows
            // Content-Transfer-Encoding 7bit, 8bit or binary.
            // Any other encoding (quoted-printable, base64) will break in
            // Gmail and AppleMail, probably elsewhere. The exact kind of breakage depends:
            // in AppleMail, only the `From`, `To`, and `Subject`
            // headers are shown inline, and the rest of the message is not visible / accessible.
            // In Gmail, it always shows as an attachment and one gets an error when clicking on it.
            //
            // So, try to re-encode the message body. If that doesn't work, the user can fallback
            // to the attachment.
            mp_builder = {
                let re_encoded = (|| {
                    let Some(original_parsed) = original_parsed else {
                        debug!("not parseable");
                        return None;
                    };
                    if original_parsed.ctype.mimetype != "text/plain" {
                        debug!("not text/plain content-type");
                        return None;
                    }
                    let mut builder = SinglePart::builder();
                    for header in &original_parsed.headers {
                        #[derive(Clone)]
                        struct RawHeader(HeaderName, String);
                        impl lettre::message::header::Header for RawHeader {
                            fn name() -> HeaderName {
                                unimplemented!("not needed, we only use display")
                            }

                            fn parse(
                                _: &str,
                            ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>>
                            {
                                unimplemented!("not needed, we only use display")
                            }

                            fn display(&self) -> lettre::message::header::HeaderValue {
                                HeaderValue::new(self.0.clone(), self.1.clone())
                            }
                        }
                        impl RawHeader {
                            fn new(hdr: &mailparse::MailHeader) -> Option<Self> {
                                let header_name = HeaderName::new_from_ascii(hdr.get_key())
                                    .ok()
                                    .or_else(|| {
                                        debug!(hdr=?hdr.get_key(), "header is not ascii");
                                        None
                                    })?;
                                let header_value = hdr.get_value_utf8().ok().or_else(|| {
                                    debug!(hdr=?hdr, "header value is not utf-8");
                                    None
                                })?;
                                Some(Self(header_name, header_value))
                            }
                        }
                        builder = builder.header(RawHeader::new(header).or_else(|| {
                            debug!("can't adapt libraries into each other");
                            None
                        })?);
                    }
                    Some(
                        builder.body(
                            Body::new_with_encoding(
                                original_parsed.get_body().ok().or_else(|| {
                                    debug!("cannot get body");
                                    None
                                })?,
                                lettre::message::header::ContentTransferEncoding::Base64,
                            )
                            .unwrap(),
                        ),
                    )
                })();

                if let Some(re_encoded) = re_encoded {
                    mp_builder.singlepart(
                        SinglePart::builder()
                            .header(ContentType::parse("message/rfc822").unwrap())
                            .header(ContentDisposition::inline())
                            // Not dangerous because we used Base64 encoding to build the `re_encoded` => EigthBit safe
                            .body(Body::dangerous_pre_encoded(
                                re_encoded.formatted(),
                                ContentTransferEncoding::EightBit,
                            )),
                    )
                } else {
                    debug!("can't inline the attachment, see previous log messages");
                    mp_builder
                }
            };

            mp_builder = mp_builder.singlepart(
                SinglePart::builder()
                    // (Stdin may not necessarily be a correct email to begin with, so, octet-stream is a reasonable default.)
                    .header(ContentType::parse("application/octet-stream").unwrap())
                    .header(ContentDisposition::attachment("stdin.eml"))
                    .body(
                        Body::new_with_encoding(
                            stdin_raw,
                            lettre::message::header::ContentTransferEncoding::Base64,
                        )
                        .unwrap(),
                    ),
            );

            mp_builder
        })
        .expect("Failed to attach stdin email message");

    debug!(
        message=%String::from_utf8_lossy(&email_message.formatted()),
        "sending message",
    );

    let smtp_transport = lettre::SmtpTransport::starttls_relay(&config.smtp_host)
        .unwrap()
        .authentication(vec![
            lettre::transport::smtp::authentication::Mechanism::Plain,
        ])
        .credentials(lettre::transport::smtp::authentication::Credentials::new(
            config.smtp_username,
            config.smtp_password,
        ))
        .build();

    let result = smtp_transport.send(&email_message);
    if result.is_ok() {
        println!("Email sent successfully");
    } else {
        println!("Failed to send email: {:?}", result);
    }
}

fn try_extract_cron_from_header(from_header_value: &str) -> Option<&str> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"(\S+) \(Cron Daemon\)").unwrap());
    match re.captures(from_header_value) {
        None => None,
        Some(caps) => Some(caps.get(1).unwrap().as_str()),
    }
}

fn escape_parens(output: &str) -> Cow<'_, str> {
    let output = Cow::Borrowed(output);
    let output = if output.contains('(') {
        Cow::Owned(output.replace('(', r"\("))
    } else {
        output
    };
    let output = if output.contains(')') {
        Cow::Owned(output.replace(')', r"\)"))
    } else {
        output
    };
    output
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use super::*;

    #[test]
    fn test_cron_from_header() {
        let f = try_extract_cron_from_header;
        assert_eq!(f("root (Cron Daemon)"), Some("root"));
        assert_eq!(f("(foo) (Cron Daemon))"), Some("(foo)"));
    }

    #[test]
    fn test_escape_parens() {
        let f = escape_parens;
        assert_eq!(
            f("foo(bar)"),
            Cow::<'_, str>::Owned(r"foo\(bar\)".to_owned())
        );
    }

    #[test]
    fn test_long_lines() {
        let msg = mailparse::parse_mail(include_bytes!("../cron-long-output.eml")).unwrap();
        assert!(matches!(
            msg.get_body_encoded(),
            mailparse::body::Body::EightBit(_)
        ));
    }
}

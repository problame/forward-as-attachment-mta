# forward-as-attachment-mta

A `sendmail` that forwards incoming mail as an attachment to a single receiver, through a single relay host.

This tool is for use on systems that aren't supposed to send mail to actual users, but, where system daemons (smartmontools, cron) etc occasionally send email that an admin should see.

Many people seem to address this use case with `nullmailer` / `ssmtp` / `msmtp` / `dma`.
However, I found that many relay hosts put requirements on `{envelope,header}x{from,to}`.

For example, I found it tricky to impossible to configure above tools to relay through AWS SES,
where the IAM user is [restricted by various policies](https://docs.aws.amazon.com/ses/latest/dg/control-user-access.html) to certain sender & receiver addresses.

To comply, one would need to configure above tools to rewrite `{envelope,header}x{from,to}`.
However, I found that hard or impossible to do, especially if the recipient address is restricted by the relayhost policy.

Hence, I wrote this tiny `sendmail` replacement.
Deploy it to your Debian using the instructions below.

## Demo

Here's how the received message look like in Mutt and Apple Mail.
We use inline attachments for convenience in the GUI.

<img width="45%" alt="screenshot mutt" src="https://github.com/problame/forward-as-attachment-mta/assets/956573/592247c9-9da0-41c2-ac19-4895a9014101">

<img width="45%" alt="screenshot" src="https://github.com/problame/forward-as-attachment-mta/assets/956573/ebf25b7f-ea5f-4fa4-b9a5-66373063d68c">



## Build

Install [`cargo deb`](https://crates.io/crates/cargo-deb).

Then
```
# build debian package
cargo deb --target x86_64-unknown-linux-musl
# list built debian package's contents
dpkg-deb -c target/x86_64-unknown-linux-musl/debian/forward-as-attachment-mta_0.1.0-1_amd64.deb
```

## Deploy

Install above `.deb` on your system.

The create the config file:

```toml
# /etc/forward-as-attachment-mta.config.toml
sender_email = "notifications@example.com"
recipient_email= "notifications@example.com"
smtp_host= "email-smtp.eu-central-1.amazonaws.com"
smtp_username= "..."
smtp_password= "..."
```

## Pre-Built Binary Packages

See GitHub releases.


## Release Process

```
version=0.2.1
gsed -i -E 's/^version =.*/version = "'"$version"'"/'  Cargo.toml
git add Cargo.toml
git commit -m "$version"
cargo deb --target x86_64-unknown-linux-musl
gh release create v$version --generate-notes ./target/x86_64-unknown-linux-musl/debian/forward-as-attachment-mta_$version-1_amd64.deb
```

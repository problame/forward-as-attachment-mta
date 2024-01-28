# forward-as-attachment-mta

A `sendmail` that forwards incoming mail as an attachment to a single receiver, through a single relay host.

This tool is for use on systems that aren't supposed to send mail to actual users, but, where system daemons (smartmontools, cron) etc occasionally send email that an admin should see.

Many people seem to address this use case with `nullmailer` / `ssmtp` / `msmtp` / `dma`.
However, I found that many relay hosts put requirements on `{envelope,header}x{from,to}`.

For example, I found it tricky to impossible to configure above tools to relay through AWS SES,
where the IAM user is [restricted by various policies](https://docs.aws.amazon.com/ses/latest/dg/control-user-access.html).

One approach to comply with such restrictions is to rewrite `{envelope,header}x{from,to}`.
However, I found that hard or impossible to configure with above tools.

Hence, I wrote my this tiny `sendmail` replacement.
Deploy it to your Debian using the instructions below.

The resulting emails sent to the specified receiver look like this:


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

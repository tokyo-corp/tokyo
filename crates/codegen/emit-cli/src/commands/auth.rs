//! Generated authentication command types.
//!
//! Runtime protocol behavior lives in `tokyo-cli-runtime::oauth`; this module
//! owns only the clap surface emitted into generated CLIs.

use proc_macro2::TokenStream;
use quote::quote;

pub(super) fn render_auth_command_types() -> TokenStream {
    quote! {
        #[derive(Debug, clap::Subcommand)]
        pub enum AuthCommand {
            /// Validate discovery, capabilities, scopes, and callback setup.
            Doctor {
                #[arg(long)]
                scheme: Option<String>,
            },
            /// Idempotently obtain a usable credential. Existing credentials
            /// are reused or refreshed; acquisition runs only when necessary.
            Ensure {
                #[arg(long)]
                scheme: Option<String>,
                /// Forbid user action, relay it to an agent, or perform it locally.
                #[arg(long, value_enum)]
                interaction: Option<AuthInteraction>,
                /// Require RFC 8628 device authorization.
                #[arg(long)]
                device: bool,
            },
            /// Authenticate interactively, or store a supplied credential.
            Login {
                #[arg(long)]
                scheme: Option<String>,
                /// Supply a credential directly. Prefer environment or hidden input.
                #[arg(long, conflicts_with = "mock")]
                token: Option<String>,
                #[arg(long)]
                device: bool,
                #[arg(long)]
                no_browser: bool,
                #[arg(long)]
                mock: bool,
                #[arg(long, requires = "mock")]
                subject: Option<String>,
                #[arg(long = "claim", requires = "mock")]
                claims: Vec<String>,
                #[arg(long, requires = "mock")]
                ttl: Option<u64>,
            },
            /// Remove one stored credential from the active profile.
            Logout {
                #[arg(long)]
                scheme: Option<String>,
            },
            /// Show credential status and a safe identity projection.
            Whoami {
                #[arg(long)]
                scheme: Option<String>,
            },
        }

        #[derive(Clone, Copy, Debug, clap::ValueEnum)]
        pub enum AuthInteraction {
            /// Never begin a flow requiring user action.
            Forbid,
            /// Emit machine-readable user-action details and keep polling.
            Relay,
            /// Permit local browser and terminal interaction.
            Allow,
        }
    }
}

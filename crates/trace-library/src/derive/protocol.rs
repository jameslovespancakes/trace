//! Protocol command tokens (PLAN decision 14, DESIGN-bridges rules 1 and 8; protocol
//! vocabulary): a message broker is a server outside the client package, so what one of
//! its commands does with its arguments has no source to derive from. Clients of a
//! command-token protocol send a command as a sequence whose element is the command's
//! token followed by its arguments (`execute_command("PUBLISH", channel, message)`).
//!
//! An `io_send` row with a `pattern` (and no `symbol`) names such a token; its `key` is the
//! position of the destination argument counted from the token (`{"pos": 1}`: the argument
//! right after it). A library call passing the token as a literal argument with a parameter
//! of the calling function at the key position makes that function
//! `Sends { channel: <row channel>, key: <that parameter> }`; the usual composition carries
//! it up to the public API. Tokens compare case-insensitively (protocol commands are
//! case-insensitive; clients spell them either way).

use super::*;

impl<'c> Program<'c> {
    /// Rule 1 over protocol command tokens: `f` passes a row's token as a literal argument
    /// followed (at the row's key offset) by one of its own parameters.
    pub(super) fn command_sends(&self, st: &mut State, f: u32, args: &[Expr]) {
        if !st.channels || args.len() < 2 {
            return;
        }
        for (t, arg) in args.iter().enumerate() {
            let Expr::Name { name, .. } = arg else { continue };
            let Some(token) = name.strip_prefix(LITERAL_PREFIX) else { continue };
            for row in self.io_send.iter().filter(|r| r.symbol.is_none()) {
                let Some(pattern) = row.pattern.as_deref() else { continue };
                if !pattern.eq_ignore_ascii_case(token) {
                    continue;
                }
                let Some(ArgSel::Pos(offset)) = row.key else { continue };
                let Some(key_arg) = args.get(t + offset as usize) else { continue };
                for v in self.eval(st, f, key_arg, 0) {
                    if let V::Param(p, i, _) = v {
                        if p == f {
                            st.add_chan(
                                f,
                                Chan::Sends {
                                    channel: row.channel_or_default(),
                                    key: i,
                                    verb: Verb::Any,
                                },
                            );
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "../../tests/unit/derive/protocol.rs"]
mod tests;

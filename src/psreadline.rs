//! PSReadLine wide-char editing fix, applied at the layer that owns the buffer.
//!
//! Windows' console line editor edits by UTF-16 *unit*, so one Backspace over
//! a non-BMP emoji (a surrogate pair) deletes half a glyph and leaves a lone
//! surrogate (`U+FFFD` on screen). Arrows step by unit too, parking the caret
//! inside a glyph. This is PSReadLine #1329 territory and it reproduces in
//! Windows Terminal and VS Code — a terminal cannot fix it from the outside.
//!
//! So foreman does not try. Instead it asks PSReadLine — which owns the input
//! buffer and always knows the truth — to bind the four affected keys to
//! surrogate-aware handlers. **No terminal-side modeling, no simulated input
//! row, no cursor prediction**: the previous attempts at those all desynced
//! (see docs/wide-chars.md).
//!
//! Injected once at pwsh spawn. Best-effort by construction: the script is a
//! no-op unless `Set-PSReadLineKeyHandler` exists, and it never fails a spawn.
//!
//! Constraints on the script text (both enforced by `wide_edit_fix_is_spawn_safe`):
//! - **no quotes** — `Session::spawn_argv` refuses quote/newline args, because
//!   the `cmd /c` shim retry would re-parse them (injection).
//! - **single line** — same reason.
//!
//! The UTF-8 encoding line is load-bearing, not hygiene: running *any*
//! ScriptBlock handler makes PSReadLine re-render the edited line through
//! `[Console]::OutputEncoding`, which defaults to a legacy codepage and would
//! render every emoji as `?` (measured — see the spike in terminal.rs tests).

/// Rebind the four unit-based edit keys to whole-glyph equivalents.
///
/// `$n = 2` exactly when the character being crossed is a surrogate pair;
/// BMP wide chars (CJK `中`) are a single UTF-16 unit and correctly keep
/// `$n = 1` — doubling them was the over-delete bug in the old terminal-side
/// approach (docs/wide-chars.md, probe #3).
pub const WIDE_EDIT_FIX: &str = concat!(
    "[Console]::OutputEncoding=[Text.Encoding]::UTF8; $OutputEncoding=[Text.Encoding]::UTF8; ",
    "if (Get-Command Set-PSReadLineKeyHandler -ErrorAction SilentlyContinue) { ",
    // Backspace: remove the WHOLE glyph. Two surrogate cases, because the caret
    // can also sit INSIDE a pair (RightArrow is deliberately not bound — see
    // below — so it steps by UTF-16 unit and can park mid-glyph):
    //   caret after a pair  -> delete the 2 units behind it
    //   caret inside a pair -> delete the 2 units straddling it
    // Anything else (incl. an active selection) delegates to the built-in,
    // which is what makes Ctrl+A + Backspace clear the line.
    "Set-PSReadLineKeyHandler -Chord Backspace -ScriptBlock { param($key,$arg) ",
    "$s=-1;$sl=0;[Microsoft.PowerShell.PSConsoleReadLine]::GetSelectionState([ref]$s,[ref]$sl); ",
    "$l=$null;$c=$null;[Microsoft.PowerShell.PSConsoleReadLine]::GetBufferState([ref]$l,[ref]$c); ",
    "if($s -lt 0 -and $c -ge 1 -and [char]::IsHighSurrogate($l[$c-1])) ",
    "{ [Microsoft.PowerShell.PSConsoleReadLine]::Delete($c-1,2); return }; ",
    "if($s -lt 0 -and $c -ge 2 -and [char]::IsLowSurrogate($l[$c-1]) -and [char]::IsHighSurrogate($l[$c-2])) ",
    "{ [Microsoft.PowerShell.PSConsoleReadLine]::Delete($c-2,2); return }; ",
    "[Microsoft.PowerShell.PSConsoleReadLine]::BackwardDeleteChar($key,$arg) }; ",
    // Delete: same two cases, forward.
    "Set-PSReadLineKeyHandler -Chord Delete -ScriptBlock { param($key,$arg) ",
    "$s=-1;$sl=0;[Microsoft.PowerShell.PSConsoleReadLine]::GetSelectionState([ref]$s,[ref]$sl); ",
    "$l=$null;$c=$null;[Microsoft.PowerShell.PSConsoleReadLine]::GetBufferState([ref]$l,[ref]$c); ",
    "if($s -lt 0 -and $c -ge 1 -and $c -lt $l.Length -and [char]::IsLowSurrogate($l[$c])) ",
    "{ [Microsoft.PowerShell.PSConsoleReadLine]::Delete($c-1,2); return }; ",
    "if($s -lt 0 -and ($c+1) -lt $l.Length -and [char]::IsHighSurrogate($l[$c]) -and [char]::IsLowSurrogate($l[$c+1])) ",
    "{ [Microsoft.PowerShell.PSConsoleReadLine]::Delete($c,2); return }; ",
    "[Microsoft.PowerShell.PSConsoleReadLine]::DeleteChar($key,$arg) }; ",
    // LeftArrow: cross the whole glyph; otherwise the built-in.
    "Set-PSReadLineKeyHandler -Chord LeftArrow -ScriptBlock { param($key,$arg) ",
    "$s=-1;$sl=0;[Microsoft.PowerShell.PSConsoleReadLine]::GetSelectionState([ref]$s,[ref]$sl); ",
    "$l=$null;$c=$null;[Microsoft.PowerShell.PSConsoleReadLine]::GetBufferState([ref]$l,[ref]$c); ",
    "if($s -lt 0 -and $c -ge 2 -and [char]::IsLowSurrogate($l[$c-1]) -and [char]::IsHighSurrogate($l[$c-2])) ",
    "{ [Microsoft.PowerShell.PSConsoleReadLine]::SetCursorPosition($c-2); return }; ",
    "[Microsoft.PowerShell.PSConsoleReadLine]::BackwardChar($key,$arg) } }",
    // RightArrow is DELIBERATELY NOT BOUND. Overriding it with a ScriptBlock
    // destroys PSReadLine's active-suggestion state, so the inline prediction
    // can no longer be accepted — even if the handler delegates straight back
    // to ForwardChar. Measured: the accepted-command test failed with the
    // delegating override in place (the shell ran the typed prefix, not the
    // suggestion). Cost of leaving it stock: Right steps one UTF-16 unit and
    // can park the caret inside an emoji — which is why Backspace/Delete above
    // both handle the caret-inside-a-pair case and still remove a whole glyph.
);

/// Spawn args that apply the fix and then hand the user an interactive shell.
/// `-Command` runs *after* the user's profile, so these bindings win; the
/// profile itself is still honored (we do not pass `-NoProfile`).
pub fn wide_edit_fix_args() -> [String; 3] {
    [
        "-NoExit".to_string(),
        "-Command".to_string(),
        WIDE_EDIT_FIX.to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Session::spawn_argv` refuses args containing quotes or newlines (the
    /// `cmd /c` shim retry would re-parse them). A violation here would break
    /// every PowerShell spawn, so pin it.
    #[test]
    fn wide_edit_fix_is_spawn_safe() {
        assert!(
            !WIDE_EDIT_FIX.contains('"'),
            "double quote breaks spawn_argv"
        );
        assert!(
            !WIDE_EDIT_FIX.contains('\''),
            "single quote breaks spawn_argv"
        );
        assert!(!WIDE_EDIT_FIX.contains('\n'), "newline breaks spawn_argv");
        assert!(!WIDE_EDIT_FIX.contains('\r'), "newline breaks spawn_argv");
    }

    /// The three keys we bind must be bound — and RightArrow must NOT be.
    /// Binding RightArrow (even to a handler that delegates to ForwardChar)
    /// destroys PSReadLine's active-suggestion state and breaks accepting an
    /// inline prediction. That regression shipped once; this pins it.
    #[test]
    fn wide_edit_fix_binds_three_keys_and_never_rightarrow() {
        for chord in ["Backspace", "Delete", "LeftArrow"] {
            assert!(
                WIDE_EDIT_FIX.contains(&format!("-Chord {chord} ")),
                "missing binding for {chord}"
            );
        }
        assert!(
            !WIDE_EDIT_FIX.contains("-Chord RightArrow"),
            "RightArrow must stay stock or inline-prediction accept breaks"
        );
        assert!(
            WIDE_EDIT_FIX
                .contains("Get-Command Set-PSReadLineKeyHandler -ErrorAction SilentlyContinue"),
            "must degrade to a no-op when PSReadLine is absent"
        );
        // The re-render encoding fix is load-bearing (emoji -> '?' without it).
        assert!(WIDE_EDIT_FIX.contains("[Console]::OutputEncoding=[Text.Encoding]::UTF8"));
        // Every handler must fall through to the built-in, or defaults (like
        // deleting an active selection) silently die.
        for builtin in [
            "BackwardDeleteChar($key,$arg)",
            "DeleteChar($key,$arg)",
            "BackwardChar($key,$arg)",
        ] {
            assert!(
                WIDE_EDIT_FIX.contains(builtin),
                "handler must delegate to {builtin}"
            );
        }
    }

    #[test]
    fn args_run_after_the_profile_and_keep_the_shell_interactive() {
        let args = wide_edit_fix_args();
        assert_eq!(args[0], "-NoExit");
        assert_eq!(args[1], "-Command");
        assert!(
            !args.contains(&"-NoProfile".to_string()),
            "profile must load"
        );
    }
}

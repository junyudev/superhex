//! Links into the playground: `superhex://story/<group>/<story>[?variant=V&play=1]`
//! (`superhex-test://` in a test build) and `superhex window story`. Both open the
//! window on the story, pick the variant, and with `play` run its `r`
//! trigger, as pressing R does. Only with Labs on.

use rune::Tx;

use super::playground;

/// Why a link opened only the window.
pub(crate) const PLAYGROUND_OFF: &str = "the playground is in Labs; turn it on with \
     `superhex labs on playground` and restart Superhex";

/// Longer links are ignored: they come from outside Superhex.
pub(super) const MAX_LINK: usize = 2048;

/// A story to open in the playground.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StoryLink {
    /// The story's `Group/Name` path, its name, or its slug.
    pub story: String,
    /// The variant, by number from 1 or by name.
    pub variant: Option<String>,
    /// Run the story's `r` trigger once it's open.
    pub play: bool,
}

impl StoryLink {
    pub(crate) fn new(story: &str, variant: Option<String>, play: bool) -> Self {
        Self {
            story: story_name(story),
            variant,
            play,
        }
    }
}

/// The scheme this build registers: a test build's never opens the real Superhex.
pub(crate) fn scheme() -> &'static str {
    if crate::app_paths::is_macos_test_build() {
        "superhex-test"
    } else {
        "superhex"
    }
}

/// Reads `scheme://story/<path>[?variant=V&play=1]`.
pub(crate) fn parse(link: &str, scheme: &str) -> Result<StoryLink, String> {
    if link.len() > MAX_LINK {
        return Err(format!("a link longer than {MAX_LINK} bytes"));
    }
    let url = url::Url::parse(link).map_err(|error| format!("not a link: {error}"))?;
    if url.scheme() != scheme {
        return Err(format!("not a {scheme}:// link"));
    }
    if url.host_str() != Some("story") {
        return Err("only story links are supported".into());
    }
    let segments: Vec<String> = url
        .path_segments()
        .into_iter()
        .flatten()
        .filter(|segment| !segment.is_empty())
        .map(percent_decode)
        .collect::<Option<_>>()
        .ok_or("the story isn't valid UTF-8")?;
    if segments.is_empty() {
        return Err("the link names no story".into());
    }
    let (mut variant, mut play) = (None, false);
    for (key, value) in url.query_pairs() {
        match &*key {
            "variant" if !value.is_empty() => variant = Some(value.into_owned()),
            "play" => play = matches!(&*value, "" | "1" | "true" | "yes"),
            _ => {}
        }
    }
    Ok(StoryLink::new(&segments.join("/"), variant, play))
}

/// Rune finds a story by path, name, or slug, and its slugs join the groups
/// and name with `-`; a slug-shaped link may use `/` there
/// (`hud/recording-your-voice`). A link made before the list nested finds
/// the story where it lists now.
fn story_name(story: &str) -> String {
    let slug_shaped = story.contains('/')
        && story
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '/');
    let story = if slug_shaped && story.contains('-') {
        story.replace('/', "-")
    } else {
        story.to_owned()
    };
    playground::alias(&story).unwrap_or(story)
}

pub(super) fn percent_decode(segment: &str) -> Option<String> {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        if bytes[i] == b'%'
            && let (Some(&high), Some(&low)) = (bytes.get(i + 1), bytes.get(i + 2))
            && let (Some(high), Some(low)) = (hex(high), hex(low))
        {
            out.push((high * 16 + low) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Opens the playground on `link`'s story; the caller opens the window.
/// With Labs off it says why and changes nothing.
pub(crate) fn open(link: &StoryLink, tx: &mut Tx) -> Result<(), &'static str> {
    if !playground::enabled() {
        return Err(PLAYGROUND_OFF);
    }
    tx.open_playground_at(&link.story, link.variant.as_deref());
    if link.play {
        tx.replay_playground();
    }
    tracing::info!(story = %link.story, variant = ?link.variant, play = link.play, "opened a playground story");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(story: &str, variant: Option<&str>, play: bool) -> StoryLink {
        StoryLink {
            story: story.into(),
            variant: variant.map(Into::into),
            play,
        }
    }

    #[test]
    fn reads_story_links() {
        assert_eq!(
            parse(
                "superhex://story/prototypes/hud/recording-your-voice?play=1",
                "superhex"
            ),
            Ok(link("prototypes-hud-recording-your-voice", None, true))
        );
        assert_eq!(
            parse(
                "superhex://story/Prototypes/HUD/Processing?variant=2",
                "superhex"
            ),
            Ok(link("Prototypes/HUD/Processing", Some("2"), false))
        );
        assert_eq!(
            parse(
                "superhex://story/Window/General/Microphone%20missing",
                "superhex"
            ),
            Ok(link("Window/General/Microphone missing", None, false))
        );
        assert_eq!(
            parse(
                "superhex://story/Prototypes/HUD/Recording%20%C2%B7%20your%20voice?variant=Quiet&play=0",
                "superhex"
            ),
            Ok(link(
                "Prototypes/HUD/Recording · your voice",
                Some("Quiet"),
                false
            ))
        );
        assert_eq!(
            parse("superhex://story/processing/", "superhex"),
            Ok(link("processing", None, false))
        );
    }

    #[test]
    fn links_from_before_the_list_nested_still_open_their_story() {
        for (old, now) in [
            (
                "superhex://story/hud/recording-your-voice?play=1",
                "prototypes-hud-recording-your-voice",
            ),
            (
                "superhex://story/HUD/Processing",
                "Prototypes/HUD/Processing",
            ),
            (
                "superhex://story/hud/processing",
                "Prototypes/hud/processing",
            ),
            (
                "superhex://story/onboarding/option-pill",
                "prototypes-onboarding-option-pill",
            ),
            ("superhex://story/mac-settings", "Window/General/Settings"),
            (
                "superhex://story/Meetings/meetings-calendar",
                "Window/Meetings/Calendar",
            ),
            (
                "superhex://story/general/mac-settings",
                "Window/General/Settings",
            ),
        ] {
            assert_eq!(
                parse(old, "superhex").map(|link| link.story),
                Ok(now.into())
            );
        }
        assert_eq!(
            StoryLink::new("HUD/Processing", None, false).story,
            "Prototypes/HUD/Processing"
        );
    }

    #[test]
    fn ignores_other_links() {
        for bad in [
            "superhex-test://story/hud/processing",
            "superhex://settings",
            "superhex://story",
            "superhex://story/",
            "superhex://story/%FF",
            "not a link",
            "",
        ] {
            assert!(parse(bad, "superhex").is_err(), "{bad}");
        }
        let long = format!("superhex://story/{}", "a".repeat(MAX_LINK));
        assert!(parse(&long, "superhex").is_err());
        assert!(parse("superhex-test://story/hud/processing", "superhex-test").is_ok());
    }
}

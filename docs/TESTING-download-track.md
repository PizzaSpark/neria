# Download track: what to test by hand

Everything here builds and passes the automated checks (`cargo test -p neria-core`,
`cargo test -p neria --test ui_rules`, fmt, clippy) and was looked at in `scripts/ui-check.ps1`
screenshots, but none of it has been run against real sites yet. Tick items off as you go.
Design choices: `design/download-track.html` (all the recommended options were picked).

## 1. Private and signed-in downloads
- [ ] Age-restricted YouTube video: the preview says "This video is age-restricted" with
      **Sign in to YouTube**. The window opens on Google's sign-in, the bar turns to
      "Signed in to YouTube" after signing in, Done closes it, the link is looked up again
      and downloads.
- [ ] `cookies/youtube.com.txt` next to the exe holds only `youtube.com` cookies (no
      `google.com` ones). `webview/youtube.com/` holds the window's browser profile.
- [ ] Members-only / private-shared-with-you video with an account that has access.
- [ ] Instagram post after hitting the anonymous limit → Sign in to Instagram → works.
- [ ] Your own YouTube "Liked videos" / "Watch later" playlists after signing in.
- [ ] Closing the sign-in window without signing in: toast "Not signed in…", nothing kept.
- [ ] Settings → Network → Signed in: the site shows with "signed in today"; **Sign out**
      removes it (and the next sign-in starts signed out). "A site" box + Sign in for a site
      typed by hand (e.g. vimeo.com).
- [ ] Import a `cookies.txt` exported by a browser extension with cookies of several sites:
      one file per site appears; a link of one of those sites downloads.
- [ ] A site that is signed in but still fails (account without access): button says
      "Sign in again to …".
- [ ] Spotify links and sites you're not signed in to still work as before; the browser
      cookies setting still works for sites without a Neria sign-in.
- [ ] Release build (`windows_subsystem = "windows"`): the sign-in window opens without a
      console window.

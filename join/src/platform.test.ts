import assert from "node:assert/strict";
import { test } from "node:test";

import { platformOf } from "./platform.js";

test("reconnaît la plateforme d'après le lien", () => {
  assert.equal(platformOf("https://teams.microsoft.com/l/meetup-join/19%3ameeting_x/0"), "teams");
  assert.equal(platformOf("https://teams.live.com/meet/9361234"), "teams");
  assert.equal(platformOf("https://meet.google.com/abc-defg-hij"), "meet");
  assert.equal(platformOf("https://evil.com/teams.microsoft.com"), "unknown");
  assert.equal(platformOf("pas un lien"), "unknown");
});

test("lien Zoom vers le client web", async () => {
  const { webClientUrl } = await import("./zoom.js");
  assert.equal(webClientUrl("https://us05web.zoom.us/j/81234567890?pwd=abc.1"), "https://us05web.zoom.us/wc/join/81234567890?pwd=abc.1");
  assert.equal(webClientUrl("https://zoom.us/j/123456789"), "https://zoom.us/wc/join/123456789");
  assert.equal(platformOf("https://us05web.zoom.us/j/81234567890?pwd=abc"), "zoom");
});

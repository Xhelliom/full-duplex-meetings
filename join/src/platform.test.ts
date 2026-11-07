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

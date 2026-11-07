import base64
import json

from fdm.agent import MeetingAgent
from fdm.config import Config


class FakeWS:
    def __init__(self):
        self.sent = []

    async def send(self, raw):
        self.sent.append(json.loads(raw))


class FakePlayer:
    def __init__(self):
        self.data = b""
        self.flushed = 0

    async def write(self, pcm):
        self.data += pcm

    async def flush(self):
        self.flushed += 1

    def is_playing(self):
        return bool(self.data)

    def played_ms(self):
        return 120


def make_agent(**kw):
    cfg = Config(api_key="x", agent_name="Nova", **kw)
    agent = MeetingAgent(cfg, capture=object(), player=FakePlayer())
    agent.ws = FakeWS()
    return agent


def test_session_update_shape():
    s = make_agent().session_update()["session"]
    assert s["audio"]["input"]["format"] == {"type": "audio/pcm", "rate": 24000}
    assert s["audio"]["input"]["turn_detection"]["create_response"] is False
    assert "plusieurs" in s["instructions"].lower()


async def test_replies_only_when_addressed():
    a = make_agent()
    await a.handle_event({"type": "conversation.item.input_audio_transcription.completed", "transcript": "on avance"})
    assert a.ws.sent == []
    await a.handle_event({"type": "conversation.item.input_audio_transcription.completed", "transcript": "Nova ?"})
    assert a.ws.sent == [{"type": "response.create"}]


async def test_audio_and_barge_in():
    a = make_agent()
    await a.handle_event({"type": "response.created"})
    await a.handle_event({"type": "response.output_audio.delta", "item_id": "it1",
                          "delta": base64.b64encode(b"\x00\x01" * 10).decode()})
    assert len(a.player.data) == 20
    await a.handle_event({"type": "input_audio_buffer.speech_started"})
    assert a.player.flushed == 1
    types = [e["type"] for e in a.ws.sent]
    assert types == ["response.cancel", "conversation.item.truncate"]
    assert a.ws.sent[1]["audio_end_ms"] == 120 and a.ws.sent[1]["item_id"] == "it1"

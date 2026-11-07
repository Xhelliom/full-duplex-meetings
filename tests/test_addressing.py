from fdm.addressing import AddressDetector


class Clock:
    def __init__(self):
        self.t = 0.0

    def __call__(self):
        return self.t


def test_name_mode_requires_mention():
    d = AddressDetector(["Assistant IA", "Nova"], mode="name", followup_seconds=10, clock=Clock())
    assert not d.should_reply("On reprend le point budget ?")
    assert d.should_reply("Nova, tu peux résumer ?")
    assert d.should_reply("Qu'en pense l'assistant ia ?")
    assert not d.should_reply("C'est novateur")  # pas de faux positif dans un mot


def test_accents_and_case():
    d = AddressDetector(["Hélène"], mode="name")
    assert d.should_reply("HELENE tu es là ?")


def test_followup_window():
    clock = Clock()
    d = AddressDetector(["Nova"], mode="name", followup_seconds=10, clock=clock)
    d.reply_finished()
    clock.t = 5
    assert d.should_reply("et pour mardi ?")
    clock.t = 16
    assert not d.should_reply("et pour mardi ?")


def test_always_mode():
    d = AddressDetector(["Nova"], mode="always")
    assert d.should_reply("bonjour")
    assert not d.should_reply("   ")

# Sanitized XU profile fixtures

These profiles preserve only public schema shape, payload length, and synthetic tail-policy bytes. They contain no captured vendor payload, serial number, firmware image, credential, or claim about the Link 2C Pro protocol.

The 52-byte fixture uses read-modify-write preservation outside its typed field. The 61-byte fixture uses a synthetic fixed nine-byte tail. Tests load both through the same strict parser used for external profiles and prove that payload length and tail handling come from the matched profile rather than a global legacy assumption.

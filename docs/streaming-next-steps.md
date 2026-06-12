# Streaming Next Steps

This tracker keeps the experimental video work tied to the stable-contract rules
in [api-stability.md](api-stability.md).

## Prototype WebRTC Signaling And Media Delivery

Status: prototype signaling slice in place; media delivery is incomplete.

The current prototype adds:

- `GET /<slug>?transport=webrtc` for the browser prototype viewer.
- `GET /<slug>/webrtc` for a JSON descriptor of the experimental surface.
- `POST /<slug>/webrtc-offer` for validating browser SDP offers.
- `--transport webrtc` as an experimental serve/lease transport selector.

The prototype intentionally keeps HID/control on the existing WebSocket path.
It does not move touch, keyboard, Home, resume, or control-claim semantics to a
WebRTC data channel.

Valid WebRTC offers currently receive a structured `501 Not Implemented`
response because Rust-side SDP answer generation, ICE/DTLS/SRTP ownership, H.264
RTP packetization, RTCP feedback, and congestion control are not implemented.
The intended media path is documented in
[streaming-video-technical-solution.md](streaming-video-technical-solution.md).

Try it locally:

```sh
simx lease --slug browser --ttl 10m --serve --port 8080 --transport webrtc
```

Then open:

```text
http://127.0.0.1:8080/browser?transport=webrtc
http://127.0.0.1:8080/browser/webrtc
```

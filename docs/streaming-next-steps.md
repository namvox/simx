# Streaming Next Steps

This tracker keeps the experimental video work tied to the stable-contract rules
in [api-stability.md](api-stability.md).

## Prototype WebRTC Signaling And Media Delivery

Status: local loopback-video milestone implemented; production readiness is incomplete.

The current prototype adds:

- `GET /<slug>?transport=webrtc` for the browser prototype viewer.
- `GET /<slug>/webrtc` for a JSON descriptor of the experimental surface.
- `POST /<slug>/webrtc-offer` for validating browser SDP offers and returning
  an SDP answer.
- `--transport webrtc` as an experimental serve/lease transport selector.

The prototype intentionally keeps HID/control on the existing WebSocket path.
It does not move touch, keyboard, Home, resume, or control-claim semantics to a
WebRTC data channel.

Valid WebRTC offers receive a `200 OK` response with an SDP answer. The viewer
keeps HID/control on the existing WebSocket path while simulator video is sent
as H.264 over a WebRTC media track. Trickle ICE, TURN, full RTCP feedback,
bitrate adaptation, WAN evidence, and production readiness are still incomplete.
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

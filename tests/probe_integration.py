"""Exercise the compiled probe in a real JVM, including lifecycle and JNI failures."""
import argparse
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import uuid


class PlaybackScenario:
    def __init__(self):
        self.phase = 0
        self.control_phase = 0

    def advance(self, packet, receiver, peer):
        current = packet["media"]
        if current and current["episode_id"] == 1 and current["position_ms"] == 12000:
            action = None
            if self.control_phase == 0 and current["playback"] == "playing":
                action = "pause"
            elif self.control_phase == 1 and current["playback"] == "paused":
                action = "play"
            elif self.control_phase == 2 and current["playback"] == "playing":
                self.control_phase = 3
            if action:
                command = dict(version=2, token=packet["token"], sequence=self.control_phase,
                               subject_id=current["subject_id"], episode_id=current["episode_id"],
                               action=action)
                receiver.sendto(json.dumps(command).encode(), peer)
                self.control_phase += 1
        matches = (
            self.phase == 0 and self.control_phase == 3
            or self.phase == 1 and current and current["playback"] == "paused" and current["position_ms"] == 45000
            or self.phase == 2 and current and current["playback"] == "buffering"
            or self.phase == 3 and current is None
            or self.phase == 4 and current and current["episode_id"] == 2 and current["position_ms"] == 1000
            or self.phase == 5 and current is None
        )
        if matches:
            self.phase += 1
        return matches

    def verify(self, packets):
        assert self.control_phase == 3, "SMTC commands did not pause/resume on the UI thread"
        assert self.phase == 6, f"Playback scenario stopped at phase {self.phase}"
        media = [p["media"] for p in packets if p["media"]]
        assert media, f"No playback snapshots: {packets}"
        assert all(m["title"] == "测试番剧" and m["duration_ms"] == 1440000 for m in media)
        assert any(m["playback"] == "paused" and m["position_ms"] == 45000 for m in media)
        assert any(m["playback"] == "buffering" for m in media)
        assert any(m["episode_id"] == 2 and m["position_ms"] == 1000 for m in media)


class EdgeScenario:
    diagnostic_phase = 9
    expected = [
        {"episode_id": 1, "playback": "playing"},
        {"playback": "stopped"},
        {"playback": "buffering"},
        {"title": "Test anime", "episode": "1 · Episode", "duration_ms": None},
        {"duration_ms": None},
        None,
        {"episode_id": 1},
        None,
        {"episode_id": 1},
        None,
        {"episode_id": 1},
        None,
        {"episode_id": 1},
        None,
        {"episode_id": 1},
        {"episode_id": 2},
        {"episode_id": 2},
        None,
    ]

    def __init__(self):
        self.phase = 0

    def advance(self, packet, receiver, peer):
        if self.phase == len(self.expected):
            return False
        current = packet["media"]
        expected = self.expected[self.phase]
        matches = current is None if expected is None else (
            current is not None and all(current[key] == value for key, value in expected.items())
        )
        if self.phase == self.diagnostic_phase:
            matches = matches and packet["diagnostic"].startswith("读取播放信息失败:")
        if matches:
            self.phase += 1
        return matches

    def verify(self, packets):
        assert self.phase == len(self.expected), f"JNI edge scenario stopped at phase {self.phase}: {packets[-3:]}"


class SpeedScenario(EdgeScenario):
    diagnostic_phase = 6
    expected = [
        {"playback_rate": 1.0, "play_when_ready": True, "playback": "playing"},
        {"playback_rate": 2.0, "play_when_ready": True, "playback": "playing"},
        {"playback_rate": 2.0, "play_when_ready": False, "playback": "paused"},
        {"playback_rate": 2.0, "play_when_ready": True, "playback": "buffering"},
        {"playback_rate": 2.0, "play_when_ready": False, "playback": "buffering"},
        {"playback_rate": 1.0},
        None,
        {"playback_rate": 1.0},
        None,
    ]


def exercise(work, probe, scenario, argument):
    log_path = work / f"jvm-{argument or 'playback'}.log"
    packets = []
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as receiver:
        receiver.bind(("127.0.0.1", 0))
        receiver.settimeout(0.5)
        token = str(uuid.uuid4())
        environment = dict(os.environ, ANIMEKO_SMTC_ENDPOINT=f"127.0.0.1:{receiver.getsockname()[1]}",
                           ANIMEKO_SMTC_TOKEN=token, JAVA_TOOL_OPTIONS=f'"-agentpath:{probe}"')
        command = ["java", "-cp", str(work), "me.him188.ani.app.domain.episode.EpisodeFetchSelectPlayState"]
        if argument:
            command.append(argument)
        try:
            with log_path.open("w", encoding="utf-8") as log:
                process = subprocess.Popen(command, env=environment, stdin=subprocess.PIPE, stdout=log, stderr=log)
                try:
                    deadline = time.monotonic() + 60
                    while process.poll() is None and time.monotonic() < deadline:
                        try:
                            data, peer = receiver.recvfrom(16384)
                        except socket.timeout:
                            continue
                        packet = json.loads(data)
                        assert packet["token"] == token
                        assert packet["version"] == 2
                        packets.append(packet)
                        if scenario.advance(packet, receiver, peer):
                            process.stdin.write(b"\n")
                            process.stdin.flush()
                    assert process.poll() is not None, f"JVM scenario timed out at phase {scenario.phase}"
                    assert process.returncode == 0, "JVM failed or crashed"
                finally:
                    if process.poll() is None:
                        process.kill()
                    process.wait(timeout=5)
                    process.stdin.close()
        finally:
            print(log_path.read_text(encoding="utf-8", errors="replace"))
    assert packets, "JVMTI VMInit did not start the probe"
    scenario.verify(packets)
    assert packets[-1]["media"] is None, "onClose must clear the active owner"
    assert [p["sequence"] for p in packets] == sorted(set(p["sequence"] for p in packets))
    print(f"Verified {len(packets)} snapshots for {argument or 'playback'}.")


def main():
    root = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--release", action="store_true")
    parser.add_argument("--target", help="Use target/<triple>/<profile> build artifacts")
    args = parser.parse_args()
    profile = "release" if args.release else "debug"
    build = root / "target"
    if args.target:
        build /= args.target
    library = {"win32": "animeko_probe.dll", "darwin": "libanimeko_probe.dylib",
               "linux": "libanimeko_probe.so"}[sys.platform]
    with tempfile.TemporaryDirectory(prefix="animeko probe ") as temporary:
        work = Path(temporary)
        probe = work / library
        shutil.copy2(build / profile / library, probe)
        fixtures = sorted((root / "tests" / "fixtures").rglob("*.java"))
        subprocess.run(["javac", "-encoding", "UTF-8", "-d", str(work), *map(str, fixtures)], check=True)
        exercise(work, probe, PlaybackScenario(), "")
        exercise(work, probe, EdgeScenario(), "edges")
        exercise(work, probe, SpeedScenario(), "speed")


if __name__ == "__main__":
    main()

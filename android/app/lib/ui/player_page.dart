import 'dart:async';

import 'package:flutter/material.dart';
import 'package:video_player/video_player.dart';

import '../format.dart';
import '../main.dart';
import '../models.dart';
import '../native.dart';

/// Plays a video or audio file while it downloads: the app's local server
/// fetches from Drive just the bytes the player asks for.
Future<void> openPlayer(BuildContext context, DriveFile file) => Navigator.of(context).push(
      MaterialPageRoute<void>(builder: (_) => PlayerPage(file: file)),
    );

class PlayerPage extends StatefulWidget {
  const PlayerPage({super.key, required this.file});
  final DriveFile file;

  @override
  State<PlayerPage> createState() => _PlayerPageState();
}

class _PlayerPageState extends State<PlayerPage> {
  VideoPlayerController? _player;
  String? _error;
  bool _controls = true;
  Timer? _hide;

  @override
  void initState() {
    super.initState();
    // Another app may be playing our stream while we are in the background.
    app.setBusy('stream', true);
    _start();
  }

  Future<void> _start() async {
    final url = app.streamUrl(widget.file);
    if (url == null) {
      setState(() => _error = t('本机服务没有启动，无法播放', 'The local server is not running'));
      return;
    }
    setState(() => _error = null);
    final player = VideoPlayerController.networkUrl(Uri.parse(url));
    _player = player;
    player.addListener(_changed);
    try {
      await player.initialize();
      if (!mounted) return;
      await player.play();
      _autoHide();
    } catch (e) {
      if (mounted) setState(() => _error = _describe(e));
    }
  }

  String _describe(Object e) {
    final text = '$e';
    return text.length > 300 ? '${text.substring(0, 300)}…' : text;
  }

  void _changed() {
    final value = _player?.value;
    if (value != null && value.hasError && _error == null) {
      _error = value.errorDescription ?? t('播放失败', 'Playback failed');
    }
    if (mounted) setState(() {});
  }

  void _autoHide() {
    _hide?.cancel();
    _hide = Timer(const Duration(seconds: 4), () {
      if (mounted && (_player?.value.isPlaying ?? false)) setState(() => _controls = false);
    });
  }

  void _toggleControls() {
    setState(() => _controls = !_controls);
    if (_controls) _autoHide();
  }

  Future<void> _retry() async {
    final old = _player;
    _player = null;
    old?.removeListener(_changed);
    await old?.dispose();
    await _start();
  }

  Future<void> _external() async {
    final url = app.streamUrl(widget.file);
    if (url == null) return;
    await _player?.pause();
    try {
      await Native.openUri(url, widget.file.mediaMime);
    } catch (e) {
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(
          SnackBar(content: Text(t('没有能播放的应用', 'No app can play this'))),
        );
      }
    }
  }

  void _seekBy(int seconds) {
    final player = _player;
    if (player == null) return;
    final target = player.value.position + Duration(seconds: seconds);
    player.seekTo(target < Duration.zero ? Duration.zero : target);
    _autoHide();
  }

  @override
  void dispose() {
    _hide?.cancel();
    _player?.removeListener(_changed);
    _player?.dispose();
    app.setBusy('stream', false);
    super.dispose();
  }

  String _clock(Duration d) {
    final h = d.inHours;
    final m = two(d.inMinutes % 60);
    final s = two(d.inSeconds % 60);
    return h > 0 ? '$h:$m:$s' : '$m:$s';
  }

  @override
  Widget build(BuildContext context) {
    final player = _player;
    final value = player?.value;
    final ready = value != null && value.isInitialized;
    final audioOnly = ready && (value.size.width == 0 || widget.file.isAudio);
    return Scaffold(
      backgroundColor: Colors.black,
      appBar: _controls || !ready
          ? AppBar(
              backgroundColor: Colors.black,
              foregroundColor: Colors.white,
              title: Text(widget.file.name, overflow: TextOverflow.ellipsis),
              actions: [
                IconButton(
                  tooltip: t('用其他应用播放', 'Play in another app'),
                  icon: const Icon(Icons.open_in_new_rounded),
                  onPressed: _external,
                ),
              ],
            )
          : null,
      body: SafeArea(
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTap: _toggleControls,
          child: Stack(
            alignment: Alignment.center,
            children: [
              if (_error != null)
                Padding(
                  padding: const EdgeInsets.all(32),
                  child: Column(mainAxisSize: MainAxisSize.min, children: [
                    const Icon(Icons.error_outline_rounded, color: Colors.white70, size: 48),
                    const SizedBox(height: 12),
                    Text(_error!, textAlign: TextAlign.center, style: const TextStyle(color: Colors.white70)),
                    const SizedBox(height: 16),
                    Wrap(spacing: 8, children: [
                      FilledButton(onPressed: _retry, child: Text(t('重试', 'Retry'))),
                      OutlinedButton(
                        onPressed: _external,
                        child: Text(t('用其他应用播放', 'Play in another app')),
                      ),
                    ]),
                  ]),
                )
              else if (!ready)
                const CircularProgressIndicator(color: Colors.white)
              else if (audioOnly)
                const Icon(Icons.audiotrack_rounded, color: Colors.white54, size: 96)
              else
                Center(
                  child: AspectRatio(aspectRatio: value.aspectRatio, child: VideoPlayer(player!)),
                ),
              if (ready && _error == null && value.isBuffering)
                const CircularProgressIndicator(color: Colors.white),
              if (ready && _error == null && (_controls || audioOnly))
                Positioned(
                  left: 0,
                  right: 0,
                  bottom: 0,
                  child: Container(
                    color: Colors.black54,
                    padding: const EdgeInsets.fromLTRB(12, 4, 12, 8),
                    child: Column(mainAxisSize: MainAxisSize.min, children: [
                      Row(children: [
                        Text(_clock(value.position), style: const TextStyle(color: Colors.white)),
                        Expanded(
                          child: Slider(
                            value: value.position.inMilliseconds
                                .clamp(0, value.duration.inMilliseconds)
                                .toDouble(),
                            max: value.duration.inMilliseconds.toDouble().clamp(1, double.infinity),
                            onChanged: (v) {
                              player!.seekTo(Duration(milliseconds: v.round()));
                              _autoHide();
                            },
                          ),
                        ),
                        Text(_clock(value.duration), style: const TextStyle(color: Colors.white)),
                      ]),
                      Row(mainAxisAlignment: MainAxisAlignment.center, children: [
                        IconButton(
                          icon: const Icon(Icons.replay_10_rounded, color: Colors.white),
                          onPressed: () => _seekBy(-10),
                        ),
                        const SizedBox(width: 12),
                        IconButton(
                          iconSize: 44,
                          icon: Icon(
                            value.isPlaying ? Icons.pause_circle_filled_rounded : Icons.play_circle_fill_rounded,
                            color: Colors.white,
                          ),
                          onPressed: () {
                            value.isPlaying ? player!.pause() : player!.play();
                            _autoHide();
                          },
                        ),
                        const SizedBox(width: 12),
                        IconButton(
                          icon: const Icon(Icons.forward_10_rounded, color: Colors.white),
                          onPressed: () => _seekBy(10),
                        ),
                      ]),
                    ]),
                  ),
                ),
            ],
          ),
        ),
      ),
    );
  }
}

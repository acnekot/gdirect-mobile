import 'dart:convert';
import 'dart:ffi';
import 'dart:isolate';

import 'package:ffi/ffi.dart';

/// A failure reported by the Rust engine.
class EngineError implements Exception {
  EngineError(this.message);
  final String message;

  @override
  String toString() => message;
}

typedef _CallNative = Pointer<Utf8> Function(Pointer<Utf8>, Pointer<Utf8>);
typedef _FreeNative = Void Function(Pointer<Utf8>);
typedef _FreeDart = void Function(Pointer<Utf8>);

String _invoke(String method, String args) {
  final lib = DynamicLibrary.open('libgdirect_mobile.so');
  final call = lib.lookupFunction<_CallNative, _CallNative>('gdm_call');
  final free = lib.lookupFunction<_FreeNative, _FreeDart>('gdm_free');
  final m = method.toNativeUtf8();
  final a = args.toNativeUtf8();
  try {
    final out = call(m, a);
    try {
      return out.toDartString();
    } finally {
      free(out);
    }
  } finally {
    calloc.free(m);
    calloc.free(a);
  }
}

/// Unwraps the engine's `{"ok": …}` / `{"error": …}` envelope.
Object? decodeReply(String reply) {
  final data = jsonDecode(reply) as Map<String, dynamic>;
  final error = data['error'];
  if (error != null) throw EngineError(error as String);
  return data['ok'];
}

/// The Rust engine (gdirect's core) behind a JSON method table.
class Engine {
  /// For methods that only read or change in-memory state: runs right here.
  static Object? fast(String method, [Map<String, Object?>? args]) =>
      decodeReply(_invoke(method, args == null ? '' : jsonEncode(args)));

  /// For methods that wait on the network: runs on a helper isolate.
  static Future<Object?> slow(String method, [Map<String, Object?>? args]) async {
    final json = args == null ? '' : jsonEncode(args);
    final reply = await Isolate.run(() => _invoke(method, json));
    return decodeReply(reply);
  }

  static Map<String, dynamic> map(Object? value) =>
      (value as Map).cast<String, dynamic>();
  static List<Map<String, dynamic>> maps(Object? value) =>
      (value as List).map((e) => (e as Map).cast<String, dynamic>()).toList();
}

import 'dart:convert';
import 'package:flutter/material.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:http/http.dart' as http;

void main() => runApp(const RemoteMobileApp());

class RemoteMobileApp extends StatelessWidget {
  const RemoteMobileApp({super.key});

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'Remote',
      debugShowCheckedModeBanner: false,
      theme: ThemeData(
        brightness: Brightness.dark,
        colorSchemeSeed: Colors.blue,
        useMaterial3: true,
      ),
      home: const DevicePage(),
    );
  }
}

class Device {
  final String id;
  final String hostname;
  final String platform;
  final bool online;
  final int lastSeen;

  Device.fromJson(Map<String, dynamic> j)
      : id = j['device_id'] as String,
        hostname = j['hostname'] as String? ?? 'Unknown',
        platform = j['platform'] as String? ?? 'unknown',
        online = j['online'] as bool? ?? false,
        lastSeen = j['last_seen_unix_ms'] as int? ?? 0;
}

class DevicePage extends StatefulWidget {
  const DevicePage({super.key});
  @override
  State<DevicePage> createState() => _DevicePageState();
}

class _DevicePageState extends State<DevicePage> {
  static const secure = FlutterSecureStorage();
  final server = TextEditingController(text: 'https://remote.example.com');
  final token = TextEditingController();
  bool loading = false;
  String? error;
  List<Device> devices = const [];

  @override
  void initState() {
    super.initState();
    _restore();
  }

  Future<void> _restore() async {
    server.text = await secure.read(key: 'server') ?? server.text;
    token.text = await secure.read(key: 'bootstrap_token') ?? '';
    if (token.text.isNotEmpty) await _refresh();
  }

  Future<void> _refresh() async {
    setState(() { loading = true; error = null; });
    try {
      await secure.write(key: 'server', value: server.text.trim());
      await secure.write(key: 'bootstrap_token', value: token.text.trim());
      final uri = Uri.parse('${server.text.trim().replaceAll(RegExp(r'/$'), '')}/api/v1/devices');
      final r = await http.get(uri, headers: {'Authorization': 'Bearer ${token.text.trim()}'});
      if (r.statusCode != 200) throw Exception('Server returned ${r.statusCode}');
      final body = jsonDecode(r.body) as List<dynamic>;
      setState(() => devices = body.map((e) => Device.fromJson(e as Map<String, dynamic>)).toList());
    } catch (e) {
      setState(() => error = e.toString());
    } finally {
      if (mounted) setState(() => loading = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('Devices'), actions: [
        IconButton(onPressed: loading ? null : _refresh, icon: const Icon(Icons.refresh)),
      ]),
      body: Column(children: [
        Padding(
          padding: const EdgeInsets.all(12),
          child: Column(children: [
            TextField(controller: server, decoration: const InputDecoration(labelText: 'Server')),
            const SizedBox(height: 8),
            TextField(controller: token, obscureText: true, decoration: const InputDecoration(labelText: 'Bootstrap/admin token (development only)')),
            if (error != null) Padding(padding: const EdgeInsets.only(top: 8), child: Text(error!, style: const TextStyle(color: Colors.redAccent))),
          ]),
        ),
        const Divider(height: 1),
        Expanded(
          child: loading && devices.isEmpty
              ? const Center(child: CircularProgressIndicator())
              : ListView.separated(
                  itemCount: devices.length,
                  separatorBuilder: (_, __) => const Divider(height: 1),
                  itemBuilder: (context, i) {
                    final d = devices[i];
                    return ListTile(
                      leading: Icon(Icons.circle, size: 12, color: d.online ? Colors.greenAccent : Colors.grey),
                      title: Text(d.hostname),
                      subtitle: Text('${d.platform} · ${d.online ? 'Online' : 'Offline'}'),
                      trailing: FilledButton(
                        onPressed: d.online ? () => Navigator.of(context).push(MaterialPageRoute(builder: (_) => ConnectPage(device: d, server: server.text.trim(), token: token.text.trim()))) : null,
                        child: const Text('Connect'),
                      ),
                    );
                  },
                ),
        ),
      ]),
    );
  }
}

class ConnectPage extends StatelessWidget {
  final Device device;
  final String server;
  final String token;
  const ConnectPage({super.key, required this.device, required this.server, required this.token});

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: Text(device.hostname)),
      body: Padding(
        padding: const EdgeInsets.all(20),
        child: Column(crossAxisAlignment: CrossAxisAlignment.stretch, children: [
          Text('● Online', style: Theme.of(context).textTheme.titleMedium),
          const SizedBox(height: 24),
          _ModeCard(
            title: 'User Screen',
            subtitle: 'Connect to the current physical display.',
            onTap: () => _showPending(context, 'user_screen'),
          ),
          const SizedBox(height: 12),
          _ModeCard(
            title: 'Admin Workspace',
            subtitle: 'Open the dedicated virtual display when supported.',
            onTap: () => _showPending(context, 'admin_workspace'),
          ),
        ]),
      ),
    );
  }

  void _showPending(BuildContext context, String mode) {
    showDialog<void>(context: context, builder: (_) => AlertDialog(
      title: const Text('WebRTC session'),
      content: Text('The mobile controller UI and WebRTC dependency are wired into the project. The next transport implementation creates a certificate-authorized $mode session and binds the incoming video track here.'),
      actions: [TextButton(onPressed: () => Navigator.pop(context), child: const Text('OK'))],
    ));
  }
}

class _ModeCard extends StatelessWidget {
  final String title;
  final String subtitle;
  final VoidCallback onTap;
  const _ModeCard({required this.title, required this.subtitle, required this.onTap});
  @override
  Widget build(BuildContext context) => Card(
    child: ListTile(
      contentPadding: const EdgeInsets.all(18),
      title: Text(title, style: Theme.of(context).textTheme.titleLarge),
      subtitle: Padding(padding: const EdgeInsets.only(top: 6), child: Text(subtitle)),
      trailing: const Icon(Icons.chevron_right),
      onTap: onTap,
    ),
  );
}

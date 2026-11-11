import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import time


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description='Compare PhysX 4.1.2 terrain, query and articulation captures with same-run SDK truth.')
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--cli', type=Path, default=Path('target/debug/pvd-cli.exe'))
    parser.add_argument('--producers', type=Path, default=Path('.deps/physx41-sample-build/Release'))
    parser.add_argument('--desktop', type=Path)
    parser.add_argument('--port', type=int, default=15427)
    parser.add_argument('--cargo-config', action='append', default=[])
    args = parser.parse_args()
    if not 1 <= args.port <= 65535:
        parser.error('--port must be between 1 and 65535')
    root = Path(__file__).resolve().parents[2]
    output = args.output.resolve()
    cli = args.cli.resolve()
    producers = args.producers.resolve()
    programs = {
        'heightfield': producers / 'pvd-physx41-heightfield.exe',
        'query-shapes': producers / 'pvd-physx41-query-shapes.exe',
        'articulation': producers / 'pvd-physx41-articulation.exe',
    }
    for path in [cli, *programs.values(), *([args.desktop.resolve()] if args.desktop else [])]:
        if not path.is_file():
            parser.error(f'Missing executable: {path}')
    output.mkdir(parents=True, exist_ok=False)
    report = dict(passed=False, expected_sdk='4.1.2', programs={str(p): digest(p) for p in [cli, *programs.values()]},
                  commands=[], captures=[], images=[], desktop_tests='pending' if args.desktop else 'skipped',
                  visual_inspection='pending' if args.desktop else 'not-requested')
    flags = getattr(subprocess, 'CREATE_NO_WINDOW', 0)
    cargo = ['cargo', 'test', '--locked']
    for config in args.cargo_config:
        cargo.extend(['--config', config])
    cargo_options = list(cargo)
    cargo.extend(['-p', 'pvd-core', '-j', '2'])

    def stop_process(process):
        if process.poll() is not None:
            return
        if os.name == 'nt':
            stopped = subprocess.run(['taskkill', '/PID', str(process.pid), '/T', '/F'],
                                     stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                     creationflags=flags, timeout=15)
            if stopped.returncode and process.poll() is None:
                raise RuntimeError(f'Could not stop verification process tree {process.pid}')
        else:
            process.kill()
        process.wait(timeout=15)

    def run(name, command, env=None, timeout=600):
        command = [str(value) for value in command]
        record = dict(name=name, command=command, exit_code=None)
        report['commands'].append(record)
        with (output / f'{name}.stdout.log').open('wb') as stdout, (output / f'{name}.stderr.log').open('wb') as stderr:
            process = subprocess.Popen(command, cwd=root, env=env, stdout=stdout, stderr=stderr,
                                       creationflags=flags)
            try:
                process.wait(timeout=timeout)
            finally:
                try:
                    stop_process(process)
                finally:
                    record['exit_code'] = process.poll()
        if record['exit_code']:
            raise RuntimeError(f'{name} failed; see {output / (name + ".stderr.log")}')

    def bind_capture(capture, truth):
        if json.loads(truth.read_text())['sdk'] != '4.1.2':
            raise RuntimeError(f'Unexpected SDK version in {truth}')
        report['captures'].append(dict(path=str(capture), sha256=digest(capture), truth=str(truth), truth_sha256=digest(truth)))

    def receive(name, extension):
        receiver_log = output / f'{name}-receiver.stdout.log'
        tcp_directory = output / f'{name}-tcp'
        receiver_command = [str(cli), 'listen', f'127.0.0.1:{args.port}', str(tcp_directory), '60']
        receiver_record = dict(name=f'{name}-receiver', command=receiver_command, exit_code=None)
        report['commands'].append(receiver_record)
        with receiver_log.open('wb') as stdout, (output / f'{name}-receiver.stderr.log').open('wb') as stderr:
            receiver = subprocess.Popen(receiver_command, cwd=root, stdout=stdout, stderr=stderr, creationflags=flags)
            try:
                deadline = time.monotonic() + 10
                while 'listening:' not in receiver_log.read_text(errors='replace'):
                    if receiver.poll() is not None or time.monotonic() >= deadline:
                        raise RuntimeError('TCP receiver did not become ready; see receiver logs')
                    time.sleep(0.05)
                tcp_truth = output / f'{name}-tcp-truth.json'
                run(f'produce-{name}-tcp', [programs[name], '--host', '127.0.0.1', '--port', args.port, tcp_truth])
                code = receiver.wait(timeout=15)
                receiver_record['exit_code'] = code
                if code:
                    raise RuntimeError('TCP receiver failed; see receiver logs')
            finally:
                try:
                    stop_process(receiver)
                finally:
                    receiver_record['exit_code'] = receiver.poll()
        captures = list(tcp_directory.glob('*.pvdstream'))
        if len(captures) != 1:
            raise RuntimeError(f'Expected one completed TCP capture, found {len(captures)}')
        tcp_capture = captures[0]
        truth = tcp_capture.with_suffix('.' + extension)
        shutil.copyfile(tcp_truth, truth)
        bind_capture(tcp_capture, truth)
        run(f'verify-{name}-tcp', [cli, 'verify', tcp_capture])
        return tcp_capture

    try:
        heightfield = output / 'heightfield.pvdstream'
        queries = output / 'query-shapes.pvdstream'
        articulation = output / 'articulation.pvdstream'
        for name, capture, extension in [('heightfield', heightfield, 'heightfield.json'),
                                         ('query-shapes', queries, 'json'),
                                         ('articulation', articulation, 'articulation.json')]:
            truth = capture.with_suffix('.' + extension)
            run('produce-' + name, [programs[name], capture, truth])
            bind_capture(capture, truth)
            run('verify-' + name, [cli, 'verify', capture])
        env = os.environ.copy()
        env['PVD_HEIGHTFIELD41_CAPTURE'] = str(heightfield)
        env['PVD_QUERY_SHAPES41_CAPTURE'] = str(queries)
        env['PVD_ARTICULATION41_CAPTURE'] = str(articulation)
        test_options = ['--', '--ignored', '--test-threads=1', '--nocapture']
        run('sdk-file-truth', [*cargo, '--test', 'heightfield', '--test', 'query_shapes', 'physx41_',
                              *test_options], env, timeout=3600)
        run('articulation-file-truth', [*cargo, '--test', 'articulation41', *test_options], env)
        tcp_queries = receive('query-shapes', 'json')
        env['PVD_QUERY_SHAPES41_CAPTURE'] = str(tcp_queries)
        run('sdk-tcp-truth', [*cargo, '--test', 'query_shapes', 'physx41_', *test_options], env)
        tcp_articulation = receive('articulation', 'articulation.json')
        env['PVD_ARTICULATION41_CAPTURE'] = str(tcp_articulation)
        run('articulation-tcp-truth', [*cargo, '--test', 'articulation41', *test_options], env)
        if args.desktop:
            desktop = args.desktop.resolve()
            report['programs'][str(desktop)] = digest(desktop)
            desktop_cargo = [*cargo_options, '-p', 'pvd-desktop', '-j', '2']
            run('articulation-desktop-truth', [*desktop_cargo, 'captured_sdk41_', *test_options], env, timeout=3600)
            report['desktop_tests'] = 'passed'
            for name, capture, frame in [('heightfield', heightfield, 40), ('query-shapes', tcp_queries, 5),
                                         ('articulation-awake', tcp_articulation, 2),
                                         ('articulation-sleeping', tcp_articulation, 6)]:
                for language in ('zh-CN', 'en'):
                    image = output / f'{name}-{language}.png'
                    run('render-' + image.stem, [desktop, '--render-check', capture, image, frame, 'overview', 1440, 960, '--language', language])
                    report['images'].append(dict(path=str(image), sha256=digest(image), visual_inspection='pending'))
        report['passed'] = True
    except Exception as error:
        report['error'] = str(error)
        raise
    finally:
        (output / 'result.json').write_text(json.dumps(report, indent=2), encoding='utf-8')
    print(output)


if __name__ == '__main__':
    main()

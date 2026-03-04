(function() {
  'use strict';

  // --- State ---
  let processes = new Map();  // pid_string -> ProcessInfo
  let events = [];            // LifecycleEvent[]
  const MAX_EVENTS = 500;
  let sortColumn = 'pid';
  let sortAsc = true;
  let ws = null;

  // --- DOM refs ---
  const connectionStatus = document.getElementById('connection-status');
  const processCount = document.getElementById('process-count');
  const treeContainer = document.getElementById('tree-container');
  const processTableBody = document.querySelector('#process-table tbody');
  const eventLog = document.getElementById('event-log');

  // --- Tab switching ---
  document.querySelectorAll('.tab').forEach(tab => {
    tab.addEventListener('click', () => {
      document.querySelectorAll('.tab').forEach(t => t.classList.remove('active'));
      document.querySelectorAll('.tab-content').forEach(c => c.classList.remove('active'));
      tab.classList.add('active');
      document.getElementById('tab-' + tab.dataset.tab).classList.add('active');
    });
  });

  // --- Table sorting ---
  document.querySelectorAll('#process-table th').forEach(th => {
    th.addEventListener('click', () => {
      const col = th.dataset.sort;
      if (sortColumn === col) {
        sortAsc = !sortAsc;
      } else {
        sortColumn = col;
        sortAsc = true;
      }
      renderProcessList();
    });
  });

  // --- WebSocket ---
  function connect() {
    const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
    ws = new WebSocket(proto + '//' + location.host + '/ws/events');

    ws.onopen = function() {
      connectionStatus.textContent = 'Connected';
      connectionStatus.className = 'connected';
    };

    ws.onclose = function() {
      connectionStatus.textContent = 'Disconnected';
      connectionStatus.className = 'disconnected';
      setTimeout(connect, 2000);
    };

    ws.onerror = function() {
      ws.close();
    };

    ws.onmessage = function(e) {
      const data = JSON.parse(e.data);

      if (data.type === 'snapshot') {
        processes.clear();
        (data.processes || []).forEach(p => {
          processes.set(pidKey(p.pid), p);
        });
        renderAll();
        return;
      }

      if (data.type === 'lagged') {
        addEvent({ type: 'Lagged', detail: 'Missed ' + data.missed + ' events', timestamp: Date.now() });
        return;
      }

      handleLifecycleEvent(data);
    };
  }

  function pidKey(pid) {
    return pid.node_id + '.' + pid.local_id;
  }

  function pidDisplay(pid) {
    if (!pid) return '-';
    return '<' + pid.node_id + '.' + pid.local_id + '>';
  }

  // --- Event handling ---
  function handleLifecycleEvent(event) {
    const type = event.type;

    switch (type) {
      case 'ProcessSpawned':
        processes.set(pidKey(event.pid), {
          pid: event.pid,
          name: event.name || null,
          parent: event.parent || null,
          uptime_ms: 0,
          is_supervisor: false,
          _spawned: event.timestamp,
        });
        addEvent({ type: 'spawn', detail: pidDisplay(event.pid) + (event.name ? ' (' + event.name + ')' : ''), timestamp: event.timestamp });
        break;

      case 'ProcessExited':
        processes.delete(pidKey(event.pid));
        addEvent({ type: 'exit', detail: pidDisplay(event.pid) + ' reason=' + formatReason(event.reason), timestamp: event.timestamp });
        break;

      case 'SupervisorStarted':
        var existing = processes.get(pidKey(event.pid));
        if (existing) existing.is_supervisor = true;
        addEvent({ type: 'supervisor', detail: 'Supervisor ' + pidDisplay(event.pid) + ' started [' + event.child_ids.join(', ') + ']', timestamp: event.timestamp });
        break;

      case 'ChildStarted':
        addEvent({ type: 'spawn', detail: 'Child ' + event.child_id + ' ' + pidDisplay(event.child_pid) + ' under ' + pidDisplay(event.supervisor_pid), timestamp: event.timestamp });
        break;

      case 'ChildExited':
        addEvent({ type: 'exit', detail: 'Child ' + event.child_id + ' ' + pidDisplay(event.child_pid) + ' exited: ' + formatReason(event.reason), timestamp: event.timestamp });
        break;

      case 'ChildRestarted':
        addEvent({ type: 'restart', detail: 'Child ' + event.child_id + ' restarted ' + pidDisplay(event.old_pid) + ' -> ' + pidDisplay(event.new_pid) + ' (#' + event.restart_count + ')', timestamp: event.timestamp });
        flashTreeNode(pidKey(event.new_pid));
        break;

      case 'SupervisorMaxRestartsExceeded':
        addEvent({ type: 'exit', detail: 'Supervisor ' + pidDisplay(event.pid) + ' max restarts exceeded', timestamp: event.timestamp });
        break;
    }

    renderAll();
  }

  function formatReason(reason) {
    if (typeof reason === 'string') return reason;
    if (reason && reason.Abnormal) return reason.Abnormal;
    return JSON.stringify(reason);
  }

  function addEvent(ev) {
    events.push(ev);
    if (events.length > MAX_EVENTS) events.shift();
    renderEventLog();
  }

  // --- Render functions ---
  function renderAll() {
    updateProcessCount();
    renderTree();
    renderProcessList();
  }

  function updateProcessCount() {
    processCount.textContent = processes.size + ' process' + (processes.size !== 1 ? 'es' : '');
  }

  function renderTree() {
    // Build parent->children map
    const children = new Map();
    const roots = [];

    processes.forEach((proc, key) => {
      if (proc.parent) {
        const parentKey = pidKey(proc.parent);
        if (!children.has(parentKey)) children.set(parentKey, []);
        children.get(parentKey).push(proc);
      } else {
        roots.push(proc);
      }
    });

    treeContainer.innerHTML = '';
    roots.forEach(proc => {
      treeContainer.appendChild(buildTreeNode(proc, children));
    });

    if (roots.length === 0 && processes.size > 0) {
      // No parent info, show flat
      processes.forEach(proc => {
        treeContainer.appendChild(buildTreeNode(proc, children));
      });
    }

    if (processes.size === 0) {
      treeContainer.innerHTML = '<div style="color:#565f89;padding:20px">No processes running</div>';
    }
  }

  function buildTreeNode(proc, childrenMap) {
    const node = document.createElement('div');
    node.className = 'tree-node';
    node.dataset.pid = pidKey(proc.pid);

    const label = document.createElement('div');
    label.className = 'tree-label ' + (proc.is_supervisor ? 'supervisor' : 'process');

    const icon = proc.is_supervisor ? '\u25BC ' : '\u25CF ';
    const name = proc.name || 'process';
    const pidSpan = '<span class="tree-pid">' + pidDisplay(proc.pid) + '</span>';
    label.innerHTML = icon + name + ' ' + pidSpan;

    node.appendChild(label);

    const key = pidKey(proc.pid);
    const kids = childrenMap.get(key) || [];
    kids.forEach(child => {
      node.appendChild(buildTreeNode(child, childrenMap));
    });

    return node;
  }

  function flashTreeNode(key) {
    setTimeout(() => {
      const node = document.querySelector('[data-pid="' + key + '"] > .tree-label');
      if (node) {
        node.classList.add('flash');
        setTimeout(() => node.classList.remove('flash'), 600);
      }
    }, 50);
  }

  function renderProcessList() {
    const procs = Array.from(processes.values());
    const now = Date.now();

    procs.sort((a, b) => {
      let va, vb;
      switch (sortColumn) {
        case 'pid':
          va = a.pid.local_id; vb = b.pid.local_id;
          break;
        case 'name':
          va = a.name || ''; vb = b.name || '';
          break;
        case 'parent':
          va = a.parent ? a.parent.local_id : 0;
          vb = b.parent ? b.parent.local_id : 0;
          break;
        case 'uptime':
          va = a._spawned ? (now - a._spawned) : a.uptime_ms;
          vb = b._spawned ? (now - b._spawned) : b.uptime_ms;
          break;
        case 'type':
          va = a.is_supervisor ? 1 : 0;
          vb = b.is_supervisor ? 1 : 0;
          break;
        default:
          va = 0; vb = 0;
      }
      if (typeof va === 'string') {
        const cmp = va.localeCompare(vb);
        return sortAsc ? cmp : -cmp;
      }
      return sortAsc ? va - vb : vb - va;
    });

    processTableBody.innerHTML = procs.map(p => {
      const uptime = p._spawned ? formatUptime(now - p._spawned) : formatUptime(p.uptime_ms);
      return '<tr>' +
        '<td>' + pidDisplay(p.pid) + '</td>' +
        '<td>' + (p.name || '-') + '</td>' +
        '<td>' + pidDisplay(p.parent) + '</td>' +
        '<td>' + uptime + '</td>' +
        '<td>' + (p.is_supervisor ? 'supervisor' : 'process') + '</td>' +
        '</tr>';
    }).join('');
  }

  function formatUptime(ms) {
    if (ms < 1000) return ms + 'ms';
    const s = Math.floor(ms / 1000);
    if (s < 60) return s + 's';
    const m = Math.floor(s / 60);
    if (m < 60) return m + 'm ' + (s % 60) + 's';
    const h = Math.floor(m / 60);
    return h + 'h ' + (m % 60) + 'm';
  }

  function renderEventLog() {
    const html = events.slice().reverse().map(ev => {
      const time = new Date(ev.timestamp).toLocaleTimeString();
      const typeClass = ev.type === 'spawn' ? 'spawn' :
                        ev.type === 'exit' ? 'exit' :
                        ev.type === 'restart' ? 'restart' : 'supervisor';
      return '<div class="event-entry">' +
        '<span class="event-time">' + time + '</span>' +
        '<span class="event-type ' + typeClass + '">' + ev.type.toUpperCase() + '</span>' +
        '<span class="event-detail">' + ev.detail + '</span>' +
        '</div>';
    }).join('');
    eventLog.innerHTML = html;
  }

  // --- Init ---
  connect();
})();

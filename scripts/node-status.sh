#!/usr/bin/env bash
set -euo pipefail

EL_NODES=(
    "rotation-node1:8545"
    "rotation-node2:8555"
    "rotation-node3:8565"
)

rpc() {
    local port=$1 method=$2
    curl -sf --max-time 3 -X POST "http://localhost:$port" \
        -H 'Content-Type: application/json' \
        -d "{\"jsonrpc\":\"2.0\",\"method\":\"$method\",\"params\":[],\"id\":1}" 2>/dev/null
}

rpc_latest_block() {
    local port=$1
    curl -sf --max-time 3 -X POST "http://localhost:$port" \
        -H 'Content-Type: application/json' \
        -d '{"jsonrpc":"2.0","method":"eth_getBlockByNumber","params":["latest",false],"id":1}' 2>/dev/null
}

hex_to_dec() {
    printf '%d' "$1" 2>/dev/null || echo "?"
}

age_str() {
    local ts=$1
    local now
    now=$(date +%s)
    local diff=$(( now - ts ))
    if (( diff < 60 )); then
        echo "${diff}s ago"
    elif (( diff < 3600 )); then
        echo "$(( diff / 60 ))m ago"
    else
        echo "$(( diff / 3600 ))h ago"
    fi
}

printf '\n%-18s  %-6s  %-12s  %-10s  %-30s  %-30s  %s\n' "NODE" "PEERS" "BLOCK" "AGE" "RLPx (TCP)" "discv5 (UDP)" "PEER ID"
printf '%-18s  %-6s  %-12s  %-10s  %-30s  %-30s  %s\n' "----" "-----" "-----" "---" "----------" "------------" "-------"

for entry in "${EL_NODES[@]}"; do
    name="${entry%%:*}"
    port="${entry##*:}"

    peer_resp=$(rpc "$port" "net_peerCount")
    peers=$(hex_to_dec "$(echo "$peer_resp" | python3 -c "import sys,json; print(json.load(sys.stdin)['result'])" 2>/dev/null)")

    block_resp=$(rpc_latest_block "$port")
    block_num=$(echo "$block_resp" | python3 -c "import sys,json; r=json.load(sys.stdin)['result']; print(int(r['number'],16))" 2>/dev/null || echo "?")
    block_ts=$(echo "$block_resp" | python3 -c "import sys,json; r=json.load(sys.stdin)['result']; print(int(r['timestamp'],16))" 2>/dev/null || echo "0")
    age=$( (( block_ts > 0 )) && age_str "$block_ts" || echo "?" )

    enode=$(docker logs "$name" 2>&1 | grep -o 'enode://[^@]*@[^ ]*' | tail -1 || echo "?")

    pubkey=$(echo "$enode" | grep -o 'enode://[^@]*' | sed 's/enode:\/\///')
    pubkey_short="${pubkey:0:40}…"
    hostport=$(echo "$enode" | grep -o '@[^?]*' | sed 's/@//')
    ip="${hostport%%:*}"
    tcp_port="${hostport##*:}"
    udp_port=$(echo "$enode" | grep -o 'discport=[0-9]*' | cut -d= -f2)
    udp_port="${udp_port:-$tcp_port}"

    rlpx_addr="${ip}:${tcp_port}"
    discv5_addr="/ip4/${ip}/udp/${udp_port}"

    printf '%-18s  %-6s  %-12s  %-10s  %-30s  %-30s  %s\n' \
        "$name" "$peers" "$block_num" "$age" "$rlpx_addr" "$discv5_addr" "0x${pubkey_short}"
done

echo
echo "To ping RLPx TCP:  nc -zv <IP> <TCP_PORT>"
echo "To ping discv5 UDP: nc -zuv <IP> <UDP_PORT>"
echo

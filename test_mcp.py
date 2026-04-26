import json
import subprocess
import time
import sys
import os

# Configuration
IMAGE = "ghcr.io/spuentesp/lain:latest"
WORKSPACE = os.getcwd()

def run_tool(tool_name, arguments={}):
    """Simulates an MCP tool call via Docker stdio"""
    # Note: This is a simplified simulation of the MCP protocol
    # In a real scenario, you'd use an MCP client library
    
    payload = {
        "jsonrpc": "2.0",
        "method": "tools/call",
        "params": {
            "name": tool_name,
            "arguments": arguments
        },
        "id": 1
    }
    
    cmd = [
        "docker", "run", "-i", "--rm",
        "-v", f"{WORKSPACE}:/workspace",
        IMAGE,
        "--workspace", "/workspace"
    ]
    
    print(f"🚀 Testing tool: {tool_name}...")
    
    try:
        process = subprocess.Popen(
            cmd,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True
        )
        
        # Send initialize (required by MCP)
        init_payload = {
            "jsonrpc": "2.0",
            "method": "initialize",
            "params": {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "test", "version": "1.0"}},
            "id": 0
        }
        process.stdin.write(json.dumps(init_payload) + "\n")
        process.stdin.flush()
        
        # Read init response
        process.stdout.readline()
        
        # Send tool call
        process.stdin.write(json.dumps(payload) + "\n")
        process.stdin.flush()
        
        # Read tool response
        response_line = process.stdout.readline()
        if response_line:
            response = json.loads(response_line)
            if "error" in response:
                print(f"❌ Error: {response['error']}")
            else:
                print(f"✅ Success!")
                # Print a snippet of the result
                result = str(response.get("result", ""))
                print(f"Result snippet: {result[:200]}...")
        
        process.terminate()
        
    except Exception as e:
        print(f"💥 Failed to run tool: {e}")

def main():
    print("--- Lain MCP Tool Integration Test (Docker) ---")
    
    # Check if image exists locally or pull it
    print(f"Checking for image {IMAGE}...")
    subprocess.run(["docker", "pull", IMAGE], check=True)
    
    tools_to_test = [
        ("get_health", {}),
        ("get_agent_strategy", {}),
        ("get_layered_map", {"layer": 0}),
        ("get_master_map", {}),
        ("find_anchors", {"limit": 3}),
        ("search_symbols", {"query": "LainServer"}),
        ("list_files", {})
    ]
    
    for name, args in tools_to_test:
        run_tool(name, args)
        print("-" * 40)

if __name__ == "__main__":
    main()

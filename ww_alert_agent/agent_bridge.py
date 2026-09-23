#!/usr/bin/env python3
"""
ww_alert_agent bridge — reads alert JSON from stdin, calls Anthropic API,
writes decision JSON to stdout.

Called by ww_alert_agent::agent as:
  python3 agent_bridge.py <api_key>
  (alert JSON piped to stdin)
"""
import sys
import json
import os

def main():
    api_key = sys.argv[1] if len(sys.argv) > 1 else os.environ.get("ANTHROPIC_API_KEY", "")
    
    alerts_json = sys.stdin.read()
    payload = json.loads(alerts_json)
    
    try:
        import anthropic
    except ImportError:
        # Fallback conservative decisions
        decisions = []
        for alert in payload["alerts"]:
            sev = alert["severity"]
            if sev in ("CRITICAL", "RED"):
                decisions.append({
                    "alert_id": alert["alert_id"],
                    "action": "Escalate",
                    "reasoning": f"Fallback: {sev} severity auto-escalated.",
                    "escalate_to": "BPHS / MOH Rapid Response",
                    "escalate_notes": "Python bridge fallback — anthropic SDK not installed."
                })
            elif sev == "AMBER" and alert["z_score"] < 2.8 and alert["spectral_score"] < 0.15:
                decisions.append({
                    "alert_id": alert["alert_id"],
                    "action": "Dismiss",
                    "reasoning": "Fallback: marginal AMBER.",
                    "notes": "Within noise floor."
                })
            else:
                decisions.append({
                    "alert_id": alert["alert_id"],
                    "action": "Confirm",
                    "reasoning": "Fallback: AMBER above noise floor.",
                    "notes": "Monitor next cycle."
                })
        print(json.dumps({"decisions": decisions}))
        return

    client = anthropic.Anthropic(api_key=api_key)
    
    # Build context for the model
    alert_lines = []
    sites = {}
    for a in payload["alerts"]:
        sites.setdefault(a["site_id"], []).append(a["pathogen"])
    
    for site, analytes in sites.items():
        if len(analytes) > 1:
            alert_lines.append(f"  ⚠ Co-occurring at {site}: {', '.join(analytes)}")
    
    for a in payload["alerts"]:
        alert_lines.append(
            f"  alert_id={a['alert_id']}  pathogen={a['pathogen']}  "
            f"category={a['analyte_category']}  severity={a['severity']}  "
            f"z={a['z_score']:.3f}  spectral={a['spectral_score']:.3f}  site={a['site_id']}"
        )

    system = """You are an autonomous biosurveillance triage agent for the Belize National Wastewater-Based Epidemiology (WBE) network.

DECISION RULES (apply in order):
1. CRITICAL or RED → Escalate (name which team and why)
2. Co-occurring analytes at same site → treat as correlated source event
3. AMBER with spectral_score < 0.15 AND z_score < 2.8 → Dismiss
4. Industrial toxicant (Lead_Pb/Mercury_Hg/TCE) at Dangriga or Belmopan Industrial → Escalate
5. Illicit substance AMBER with z_score ≥ 3.0 → Escalate (ER correlation check required)
6. All other AMBER → Confirm with monitoring note

Call triage_alerts with a decision for EVERY alert_id. No alerts may be omitted."""

    user = f"TRIAGE REQUEST — {len(payload['alerts'])} open alerts:\n\n" + "\n".join(alert_lines)

    triage_tool = {
        "name": "triage_alerts",
        "description": "Submit triage decisions for all open alerts.",
        "input_schema": {
            "type": "object",
            "required": ["decisions"],
            "properties": {
                "decisions": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "required": ["alert_id", "action", "reasoning"],
                        "properties": {
                            "alert_id": {"type": "string"},
                            "action": {"type": "string", "enum": ["Confirm", "Dismiss", "Escalate"]},
                            "reasoning": {"type": "string"},
                            "notes": {"type": "string"},
                            "escalate_to": {"type": "string"},
                            "escalate_notes": {"type": "string"}
                        }
                    }
                }
            }
        }
    }

    response = client.messages.create(
        model="claude-sonnet-4-6",
        max_tokens=4096,
        system=system,
        tools=[triage_tool],
        tool_choice={"type": "any"},
        messages=[{"role": "user", "content": user}]
    )

    # Extract tool use result
    for block in response.content:
        if block.type == "tool_use" and block.name == "triage_alerts":
            print(json.dumps(block.input))
            return

    # Fallback if no tool use
    print(json.dumps({"decisions": [], "error": "no tool_use block in response"}))

if __name__ == "__main__":
    main()

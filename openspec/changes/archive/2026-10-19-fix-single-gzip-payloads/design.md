# Design

Use the existing staged payload path selection and validation.
The gzip fallback streams decoded bytes to the validated destination.
All other payload staging keeps its current copy behavior.
Strengthen the existing nested gzip regression to check decoded content.
Verify real Diversion installation, command execution, and removal.
Keep the failed end-to-end trial as evidence.

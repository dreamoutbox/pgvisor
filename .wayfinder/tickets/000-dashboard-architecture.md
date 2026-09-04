---
id: "000-dashboard-architecture"
title: "Dashboard Architecture"
type: "grilling"
status: "closed"
assignee: "antigravity"
blocked_by: []
---

## Question

Where should the Axum + Askama web dashboard be hosted?

## Resolution

Embedded in the Proxy service as a central gateway for viewing cluster topology, inspecting node metrics and configurations, managing OpenDAL backups, and accessing direct SQL query execution.

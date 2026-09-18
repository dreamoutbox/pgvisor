import os
import sys
import unittest
from unittest.mock import MagicMock, patch

# Add parent directory to sys.path
sys.path.insert(0, os.path.abspath(os.path.join(os.path.dirname(__file__), "..")))

from app import create_app
from models.item import DemoItem


class TestDemoApp(unittest.TestCase):
    """Unit tests for the PgVisor demo application routes and models."""

    def setUp(self):
        self.app = create_app()
        self.app.config["TESTING"] = True
        self.client = self.app.test_client()

    @patch("models.cluster.ClusterModel.get_cluster_info")
    @patch("models.item.DemoItem.all")
    @patch("models.item.DemoItem.count_by_status")
    def test_index_route(self, mock_counts, mock_all, mock_cluster):
        mock_counts.return_value = {"Active": 1, "Pending": 0, "Archived": 0, "Total": 1}
        mock_all.return_value = [
            DemoItem(id=1, title="Test Item", category="Test", description="Test Desc", status="Active")
        ]
        mock_cluster.return_value = {
            "connected": True,
            "node_role": "Leader (Primary)",
            "db_name": "postgres",
            "db_user": "postgres",
            "server_addr": "127.0.0.1",
            "server_port": 5432,
            "in_recovery": False,
            "pg_version": "PostgreSQL 18.0",
        }

        response = self.client.get("/")
        self.assertEqual(response.status_code, 200)
        self.assertIn(b"Test Item", response.data)
        self.assertIn(b"Items Management", response.data)
        self.assertIn(b"Proxy 5432 Online", response.data)

    @patch("models.item.DemoItem.create")
    def test_create_item_route(self, mock_create):
        mock_create.return_value = DemoItem(
            id=42, title="New Task", category="Ops", description="A new task", status="Active"
        )
        response = self.client.post("/items/create", data={
            "title": "New Task",
            "category": "Ops",
            "description": "A new task",
            "status": "Active"
        }, follow_redirects=True)
        self.assertEqual(response.status_code, 200)
        mock_create.assert_called_once_with(
            title="New Task", category="Ops", description="A new task", status="Active"
        )

    @patch("models.cluster.ClusterModel.get_cluster_info")
    def test_health_endpoint_healthy(self, mock_cluster):
        mock_cluster.return_value = {
            "connected": True,
            "node_role": "Leader (Primary)",
            "db_name": "postgres",
        }
        response = self.client.get("/api/health")
        self.assertEqual(response.status_code, 200)
        json_data = response.get_json()
        self.assertEqual(json_data["status"], "healthy")
        self.assertTrue(json_data["database"]["connected"])

    @patch("models.cluster.ClusterModel.get_cluster_info")
    def test_health_endpoint_unhealthy(self, mock_cluster):
        mock_cluster.return_value = {
            "connected": False,
            "error": "Connection refused",
        }
        response = self.client.get("/api/health")
        self.assertEqual(response.status_code, 503)
        json_data = response.get_json()
        self.assertEqual(json_data["status"], "unhealthy")

    @patch("models.cluster.ClusterModel.get_cluster_info")
    @patch("models.cluster.ClusterModel.test_routing")
    def test_cluster_diagnostic_route(self, mock_routing, mock_cluster):
        mock_cluster.return_value = {
            "connected": True,
            "node_role": "Leader (Primary)",
            "db_name": "postgres",
            "db_user": "postgres",
            "server_addr": "127.0.0.1",
            "server_port": 5432,
            "in_recovery": False,
            "pg_version": "PostgreSQL 18.0",
        }
        mock_routing.return_value = {
            "read_test": {
                "server_addr": "10.0.0.2",
                "server_port": 5432,
                "in_recovery": True,
                "routed_role": "Standby Replica",
                "query_time": "2026-09-18 12:00:00",
            },
            "write_test": {
                "server_addr": "10.0.0.1",
                "server_port": 5432,
                "in_recovery": False,
                "routed_role": "Leader Primary",
            },
            "error": None,
        }
        response = self.client.get("/cluster?run_test=1")
        self.assertEqual(response.status_code, 200)
        self.assertIn(b"Live Routing Test Results", response.data)
        self.assertIn(b"Standby Replica", response.data)
        self.assertIn(b"Leader Primary", response.data)


if __name__ == "__main__":
    unittest.main()

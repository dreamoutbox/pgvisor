import os
from dotenv import load_dotenv

load_dotenv()

class Config:
    """Application configuration."""

    SECRET_KEY = os.getenv("SECRET_KEY", "dev-secret-key-pgvisor-demo")
    DEBUG = os.getenv("DEBUG", "false").lower() in ("true", "1", "yes")
    PORT = int(os.getenv("PORT", "5000"))

    # Database connection parameters
    DB_HOST = os.getenv("DB_HOST", "127.0.0.1")
    DB_PORT = int(os.getenv("DB_PORT", "5432"))
    DB_NAME = os.getenv("DB_NAME", "postgres")
    DB_USER = os.getenv("DB_USER", "postgres")
    DB_PASSWORD = os.getenv("DB_PASSWORD", "postgres")

    # Optional full DATABASE_URL
    DATABASE_URL = os.getenv("DATABASE_URL")

    @classmethod
    def get_dsn(cls) -> str:
        """Return PostgreSQL DSN string for psycopg2 connection."""
        if cls.DATABASE_URL:
            return cls.DATABASE_URL
        return (
            f"host={cls.DB_HOST} "
            f"port={cls.DB_PORT} "
            f"dbname={cls.DB_NAME} "
            f"user={cls.DB_USER} "
            f"password={cls.DB_PASSWORD}"
        )

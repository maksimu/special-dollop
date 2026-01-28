-- PostgreSQL initialization script for integration tests

-- Create test table
CREATE TABLE IF NOT EXISTS users (
    id SERIAL PRIMARY KEY,
    username VARCHAR(50) NOT NULL,
    email VARCHAR(100) NOT NULL,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    is_active BOOLEAN DEFAULT TRUE
);

-- Insert test data
INSERT INTO users (username, email, is_active) VALUES
    ('alice', 'alice@example.com', TRUE),
    ('bob', 'bob@example.com', TRUE),
    ('charlie', 'charlie@example.com', FALSE);

-- Create products table with various PostgreSQL types
CREATE TABLE IF NOT EXISTS products (
    id SERIAL PRIMARY KEY,
    name VARCHAR(100) NOT NULL,
    price NUMERIC(10, 2) NOT NULL,
    quantity INTEGER DEFAULT 0,
    metadata JSONB,
    tags TEXT[],
    created_at TIMESTAMPTZ DEFAULT NOW()
);

INSERT INTO products (name, price, quantity, metadata, tags) VALUES
    ('Widget A', 19.99, 100, '{"color": "red", "size": "medium"}', ARRAY['electronics', 'popular']),
    ('Widget B', 29.99, 50, '{"color": "blue", "size": "large"}', ARRAY['electronics']),
    ('Gadget X', 99.99, 25, '{"warranty": "2 years"}', ARRAY['premium', 'new']);

-- Create a view for testing
CREATE OR REPLACE VIEW active_users AS
SELECT id, username, email FROM users WHERE is_active = TRUE;

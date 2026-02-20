-- MySQL/MariaDB initialization script for integration tests

-- Create test table
CREATE TABLE IF NOT EXISTS users (
    id INT AUTO_INCREMENT PRIMARY KEY,
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

-- Create products table
CREATE TABLE IF NOT EXISTS products (
    id INT AUTO_INCREMENT PRIMARY KEY,
    name VARCHAR(100) NOT NULL,
    price DECIMAL(10, 2) NOT NULL,
    quantity INT DEFAULT 0
);

INSERT INTO products (name, price, quantity) VALUES
    ('Widget A', 19.99, 100),
    ('Widget B', 29.99, 50),
    ('Gadget X', 99.99, 25);

-- Grant permissions to test user
GRANT ALL PRIVILEGES ON testdb.* TO 'testuser'@'%';
FLUSH PRIVILEGES;

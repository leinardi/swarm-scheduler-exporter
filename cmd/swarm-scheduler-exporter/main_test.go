/*
 * MIT License
 *
 * Copyright (c) 2025 Roberto Leinardi
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to deal
 * in the Software without restriction, including without limitation the rights
 * to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
 * copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in all
 * copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
 * OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
 * SOFTWARE.
 */

package main

import (
	"errors"
	"testing"

	"github.com/leinardi/swarm-scheduler-exporter/internal/collector"
)

func TestStringSlice_Set_EmptyErrors(t *testing.T) {
	var s stringSlice

	err := s.Set("")
	if !errors.Is(err, ErrEmptyFlagValue) {
		t.Errorf("expected ErrEmptyFlagValue, got %v", err)
	}

	if len(s) != 0 {
		t.Error("empty value must not be appended")
	}
}

func TestStringSlice_Set_Appends(t *testing.T) {
	var s stringSlice

	err := s.Set("a")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	err = s.Set("b")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	if len(s) != 2 || s[0] != "a" || s[1] != "b" {
		t.Errorf("unexpected slice: %v", s)
	}
}

func TestStringSlice_String(t *testing.T) {
	s := stringSlice{"x", "y"}

	got := s.String()
	if got == "" {
		t.Error("String() should not return empty for non-empty slice")
	}
}

func TestValidateAndSetCustomLabels_Valid(t *testing.T) {
	t.Cleanup(func() { collector.SetCustomLabels(nil, nil) })

	err := validateAndSetCustomLabels([]string{"team", "tier"})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
}

func TestValidateAndSetCustomLabels_TooMany(t *testing.T) {
	t.Cleanup(func() { collector.SetCustomLabels(nil, nil) })
	// 9 labels exceeds the max of 8.
	labels := []string{"a", "b", "c", "d", "e", "f", "g", "h", "i"}

	err := validateAndSetCustomLabels(labels)
	if err == nil {
		t.Error("expected error for too many labels")
	}
}

func TestValidateAndSetCustomLabels_InvalidName_ReservedPrefix(t *testing.T) {
	t.Cleanup(func() { collector.SetCustomLabels(nil, nil) })

	err := validateAndSetCustomLabels([]string{"__reserved"})
	if err == nil {
		t.Error("expected error for __ prefix label")
	}
}

func TestValidateAndSetCustomLabels_PostSanitizeCollision(t *testing.T) {
	t.Cleanup(func() { collector.SetCustomLabels(nil, nil) })

	err := validateAndSetCustomLabels([]string{"foo.bar", "foo-bar"})
	if err == nil {
		t.Error("expected error for post-sanitize collision")
	}
}

func TestValidateAndSetCustomLabels_Empty(t *testing.T) {
	t.Cleanup(func() { collector.SetCustomLabels(nil, nil) })

	err := validateAndSetCustomLabels(nil)
	if err != nil {
		t.Fatalf("unexpected error for nil input: %v", err)
	}
}
